package resume

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os/exec"
	"path"
	"slices"
	"strings"
	"time"

	"github.com/pkg/sftp"
	"github.com/rclone/rclone/fs"
)

// A copy inside one sftp server is made by the server itself, with its own cp, run
// through the remote's ssh command as the sftp sessions are. sftp has no copy, and
// rclone's backend makes one only as a hard link, so the file would otherwise be
// downloaded and uploaded again.
//
// The command is an sh script handed to the login shell as exec /bin/sh -c '…', which
// every common one passes on unchanged: the script holds no single quote, no ! and no
// backslash but before a digit, and the paths are spelt in octal escapes, so any name
// passes. Its input is held open, and a watcher in the script ends cp when it closes,
// which is how a stopped job stops the copy on the server.

const (
	// shProbe is what a server with sh answers when asked.
	shProbe = "rse-sh"
	// shProbeTime is how long a server may take to answer.
	shProbeTime = 10 * time.Second
	// stopTime is how long a stopped cp may take to end before its ssh is killed.
	stopTime = 10 * time.Second
)

// errNoServerCopy is returned when the server cannot copy by itself: the file is then
// copied through this machine.
var errNoServerCopy = errors.New("the server cannot copy by itself")

// serverCopier returns the target when it can ask its server to copy a file.
func (j *job) serverCopier() *sftpTarget {
	t, _ := j.target.(*sftpTarget)
	return t
}

// hasSh says whether the server runs /bin/sh, asked once. A server that does not
// answer is asked again for the next file.
func (t *sftpTarget) hasSh(ctx context.Context) bool {
	t.mu.Lock()
	known := t.sh
	t.mu.Unlock()
	if known != nil {
		return *known
	}
	ctx, cancel := context.WithTimeout(ctx, shProbeTime)
	defer cancel()
	cmd := exec.CommandContext(ctx, t.opt.SSH[0], append(slices.Clone(t.opt.SSH[1:]), shCommand("echo "+shProbe))...)
	out, err := cmd.Output()
	if ctx.Err() != nil {
		return false
	}
	answer := err == nil && strings.TrimSpace(string(out)) == shProbe
	t.mu.Lock()
	t.sh = &answer
	t.mu.Unlock()
	return answer
}

// shCommand is the command line that has a login shell run script with sh.
func shCommand(script string) string {
	return "exec /bin/sh -c '" + script + "'"
}

// assign sets the shell variable name to value, spelt in octal. The x keeps a value
// ending in a line break whole through $(…).
func assign(script *strings.Builder, name, value string) {
	fmt.Fprintf(script, `%s=$(printf "%%b" "`, name)
	for i := range len(value) {
		fmt.Fprintf(script, `\0%03o`, value[i])
	}
	fmt.Fprintf(script, `"; echo x); %s="${%s%%x}"; `, name, name)
}

// copyScript is the script that copies the server's file from to the file to, which is
// removed first: a link there would have cp write what it leads to.
func copyScript(from, to string) string {
	var script strings.Builder
	// The watcher: when standard input closes, it ends this shell, or the cp that exec
	// has put in its place.
	script.WriteString(`exec 3<&0; ( read x <&3; kill $$ ) >/dev/null 2>&1 & `)
	assign(&script, "s", from)
	assign(&script, "d", to)
	script.WriteString(`rm -f -- "$d" && exec cp -p -- "$s" "$d"`)
	return script.String()
}

// serverPathIn is the server's path for remote in the fs of root, as rclone's sftp
// backend resolves it: a relative root is relative to the login directory.
func (t *sftpTarget) serverPathIn(c *sftp.Client, root, remote string) (string, error) {
	abs := t.opt.Enc.FromStandardPath(root)
	if !path.IsAbs(abs) {
		resolved, err := c.RealPath(abs)
		if err != nil {
			return "", fmt.Errorf("sftp: cannot resolve %q: %w", abs, err)
		}
		abs = resolved
	}
	return path.Join(abs, t.opt.Enc.FromStandardPath(remote)), nil
}

// copyOnServer copies src, a file of this server, to the file part of the target's fs,
// with the server's own cp. It returns errNoServerCopy when the server has no sh to
// run it, and the context's error when the job is stopped, after cp has ended.
func (t *sftpTarget) copyOnServer(ctx context.Context, src fs.Object, part string) error {
	if !t.hasSh(ctx) {
		return errNoServerCopy
	}
	c, err := t.get()
	if err != nil {
		return err
	}
	from, err := t.serverPathIn(c.client, src.Fs().Root(), src.Remote())
	var to string
	if err == nil {
		to, err = t.serverPath(c.client, part)
	}
	t.put(c, err)
	if err != nil {
		return err
	}

	args := append(slices.Clone(t.opt.SSH[1:]), shCommand(copyScript(from, to)))
	// Not tied to the job's context, which would kill ssh and leave cp running on the
	// server: a stopped job closes the input instead, and waits for cp to end.
	cmd := exec.Command(t.opt.SSH[0], args...)
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	fs.Debugf(src, "rse: copying on the server to %s", to)
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("ssh: %w", err)
	}
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	select {
	case err = <-done:
		_ = stdin.Close()
	case <-ctx.Done():
		_ = stdin.Close()
		select {
		case <-done:
		case <-time.After(stopTime):
			_ = cmd.Process.Kill()
			<-done
		}
		return ctx.Err()
	}
	if err != nil {
		said := strings.TrimSpace(stderr.String())
		if said == "" {
			said = err.Error()
		}
		return fmt.Errorf("copying on the server: %s", said)
	}
	return nil
}
