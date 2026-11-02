package resume

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/pkg/sftp"
	"github.com/rclone/rclone/backend/local"
	rsftp "github.com/rclone/rclone/backend/sftp"
	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/config/configmap"
	"github.com/rclone/rclone/fs/config/configstruct"
	"github.com/rclone/rclone/lib/encoder"
)

// A target writes the partial files of one fs, which rclone's backends cannot append to.
type target interface {
	// open opens remote, relative to the fs's root, for writing at off, after cutting
	// it there. The file is created if missing; its directory must exist.
	open(ctx context.Context, remote string, off int64) (io.WriteCloser, error)
	// slack is how many bytes before the end of a partial file an interrupted write may
	// have left unwritten: resuming starts that far back.
	slack() int64
	// close releases what the target keeps open between files.
	close()
}

// configOf reads the backend and the config of an fs string, from rclone.conf and the
// string's own parameters, as rclone does for NewFs.
func configOf(fsString string) (*fs.RegInfo, configmap.Mapper, error) {
	info, _, _, m, err := fs.ConfigFs(fsString)
	return info, m, err
}

// targetFor returns how to write partial files on f, or nil when they cannot be
// appended to.
func targetFor(f fs.Fs) (target, error) {
	info, m, err := configOf(fs.ConfigStringFull(f))
	if err != nil {
		return nil, err
	}
	return newTarget(info, m, f.Root())
}

// newTarget returns how to write partial files under root on the fs info and m
// configure, or nil when they cannot be appended to: local disks, and sftp servers
// reached through an ssh command, which an sftp client of our own can run too.
func newTarget(info *fs.RegInfo, m configmap.Mapper, root string) (target, error) {
	switch info.Name {
	case "local":
		var opt local.Options
		if err := configstruct.Set(m, &opt); err != nil {
			return nil, err
		}
		return &localTarget{root: filepath.FromSlash(opt.Enc.FromStandardPath(root)), enc: opt.Enc}, nil
	case "sftp":
		opt := new(rsftp.Options)
		if err := configstruct.Set(m, opt); err != nil {
			return nil, err
		}
		if len(opt.SSH) == 0 {
			return nil, nil
		}
		return &sftpTarget{opt: opt, root: root}, nil
	}
	return nil, nil
}

// localTarget appends to files on this machine.
type localTarget struct {
	root string
	enc  encoder.MultiEncoder
}

func (t *localTarget) open(_ context.Context, remote string, off int64) (io.WriteCloser, error) {
	name := filepath.Join(t.root, filepath.FromSlash(t.enc.FromStandardPath(remote)))
	f, err := os.OpenFile(name, os.O_WRONLY|os.O_CREATE, 0o666)
	if err != nil {
		return nil, err
	}
	if err = f.Truncate(off); err == nil {
		_, err = f.Seek(off, io.SeekStart)
	}
	if err != nil {
		_ = f.Close()
		return nil, err
	}
	return f, nil
}

// Writes to a local file go out in order: whatever is there was written.
func (t *localTarget) slack() int64 { return 0 }

func (t *localTarget) close() {}

// sftpTarget writes over sftp sessions of its own, which run the remote's ssh command
// as rclone's sftp backend does (backend/sftp/ssh_external.go), so a master connection
// the command shares is used for them too. A session is kept for the next file once
// one is written.
type sftpTarget struct {
	opt  *rsftp.Options
	root string

	mu      sync.Mutex
	absRoot string // root as the server names it, once known
	idle    []*sshConn
}

func (t *sftpTarget) open(_ context.Context, remote string, off int64) (io.WriteCloser, error) {
	c, err := t.get()
	if err != nil {
		return nil, err
	}
	name, err := t.serverPath(c.client, remote)
	if err != nil {
		t.put(c, err)
		return nil, err
	}
	f, err := c.client.OpenFile(name, os.O_WRONLY|os.O_CREATE)
	if err != nil {
		t.put(c, err)
		return nil, fmt.Errorf("sftp open %q: %w", name, err)
	}
	if err = f.Truncate(off); err == nil {
		_, err = f.Seek(off, io.SeekStart)
	}
	if err != nil {
		_ = f.Close()
		t.put(c, err)
		return nil, fmt.Errorf("sftp truncate %q: %w", name, err)
	}
	return &sftpFile{File: f, conn: c, target: t}, nil
}

// get takes an idle session, or starts one.
func (t *sftpTarget) get() (*sshConn, error) {
	t.mu.Lock()
	if n := len(t.idle); n > 0 {
		c := t.idle[n-1]
		t.idle = t.idle[:n-1]
		t.mu.Unlock()
		return c, nil
	}
	t.mu.Unlock()
	return t.connect()
}

// put keeps c for the next file, unless its last use failed: the session may be gone.
func (t *sftpTarget) put(c *sshConn, err error) {
	if err != nil {
		c.close()
		return
	}
	t.mu.Lock()
	t.idle = append(t.idle, c)
	t.mu.Unlock()
}

func (t *sftpTarget) close() {
	t.mu.Lock()
	idle := t.idle
	t.idle = nil
	t.mu.Unlock()
	for _, c := range idle {
		c.close()
	}
}

// With concurrent writes, the requests in flight when a session ends may have been
// carried out in any order, leaving a hole before the end.
func (t *sftpTarget) slack() int64 {
	chunk, n := int64(t.opt.ChunkSize), int64(t.opt.Concurrency)
	if chunk <= 0 {
		chunk = 32 * 1024
	}
	if n <= 0 {
		n = 64
	}
	if t.opt.DisableConcurrentWrites {
		return chunk
	}
	return chunk * n
}

// serverPath is the server's path for remote, as rclone's sftp backend resolves it: a
// relative root is relative to the login directory.
func (t *sftpTarget) serverPath(c *sftp.Client, remote string) (string, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.absRoot == "" {
		abs := t.opt.Enc.FromStandardPath(t.root)
		if !path.IsAbs(abs) {
			if resolved, err := c.RealPath(abs); err == nil {
				abs = resolved
			} else if cwd, err := c.Getwd(); err == nil {
				abs = path.Join(cwd, abs)
			} else {
				return "", fmt.Errorf("sftp: cannot resolve %q: %w", abs, err)
			}
		}
		t.absRoot = abs
	}
	return path.Join(t.absRoot, t.opt.Enc.FromStandardPath(remote)), nil
}

// sshConn is one ssh process running the sftp server, and our client of it.
type sshConn struct {
	cmd    *exec.Cmd
	client *sftp.Client
	stderr *tailBuffer
}

func (t *sftpTarget) connect() (*sshConn, error) {
	args := slices.Clone(t.opt.SSH[1:])
	if t.opt.ServerCommand != "" {
		args = append(args, t.opt.ServerCommand)
	} else {
		args = append(args, "-s", t.opt.Subsystem)
	}
	// Not tied to the job's context: a stopped job closes the file first, so that the
	// writes in flight finish and the partial file ends where they did.
	cmd := exec.Command(t.opt.SSH[0], args...)
	stderr := &tailBuffer{max: 4096}
	cmd.Stderr = stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}
	fs.Debugf(nil, "rse: running %v", fs.SpaceSepList(cmd.Args))
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("ssh: %w", err)
	}
	client, err := sftp.NewClientPipe(stdout, stdin,
		sftp.UseFstat(t.opt.UseFstat),
		sftp.UseConcurrentWrites(!t.opt.DisableConcurrentWrites),
		sftp.MaxPacketUnchecked(int(t.opt.ChunkSize)),
		sftp.MaxConcurrentRequestsPerFile(t.opt.Concurrency),
	)
	c := &sshConn{cmd: cmd, client: client, stderr: stderr}
	if err != nil {
		c.close()
		return nil, fmt.Errorf("sftp over %s: %w%s", t.opt.SSH[0], err, stderr.suffix())
	}
	return c, nil
}

// close ends the session and waits a little for ssh to exit, then kills it.
func (c *sshConn) close() {
	if c.client != nil {
		_ = c.client.Close()
	}
	done := make(chan struct{})
	go func() {
		_ = c.cmd.Wait()
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		_ = c.cmd.Process.Kill()
		<-done
	}
}

// sftpFile is a file open for writing in a session of target's.
type sftpFile struct {
	*sftp.File
	conn   *sshConn
	target *sftpTarget
	failed error
}

// ReadFrom writes concurrently. After an error, the writes that went through may not
// end where the file now does, so it is cut back to the first failed one.
func (f *sftpFile) ReadFrom(r io.Reader) (int64, error) {
	n, err := f.File.ReadFrom(r)
	if err != nil {
		if pos, serr := f.File.Seek(0, io.SeekCurrent); serr == nil {
			f.failed = f.File.Truncate(pos)
		}
	}
	return n, err
}

func (f *sftpFile) Close() error {
	err := f.File.Close()
	f.target.put(f.conn, errors.Join(f.failed, err))
	return err
}

// tailBuffer keeps the last max bytes written to it, for ssh's error messages.
type tailBuffer struct {
	mu  sync.Mutex
	max int
	buf []byte
}

func (b *tailBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.buf = append(b.buf, p...)
	if len(b.buf) > b.max {
		b.buf = b.buf[len(b.buf)-b.max:]
	}
	return len(p), nil
}

// suffix is what ssh said, to append to an error, or "".
func (b *tailBuffer) suffix() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	s := strings.TrimSpace(string(bytes.ToValidUTF8(b.buf, nil)))
	if s == "" {
		return ""
	}
	return ": " + s
}
