package resume

import (
	"bytes"
	"context"
	"crypto/rand"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/pkg/sftp"
	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/accounting"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/fs/rc/jobs"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	_ "github.com/rclone/rclone/backend/memory"
	_ "github.com/rclone/rclone/fs/operations" // core/bwlimit
)

// The test binary is also the ssh command of the sftp remotes: run with
// RSE_FAKE_SSH set, it serves sftp on stdin and stdout for "-s sftp", and runs any other
// command with sh, in the directory RSE_FAKE_SSH_HOME, which stands for the login
// directory.
func TestMain(m *testing.M) {
	if os.Getenv("RSE_FAKE_SSH") != "" {
		os.Exit(fakeSSH(os.Args[1:]))
	}
	accounting.Start(context.Background())
	os.Exit(m.Run())
}

func fakeSSH(args []string) int {
	if err := os.Chdir(os.Getenv("RSE_FAKE_SSH_HOME")); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 255
	}
	if len(args) >= 2 && args[len(args)-2] == "-s" {
		server, err := sftp.NewServer(struct {
			io.Reader
			io.WriteCloser
		}{os.Stdin, os.Stdout})
		if err == nil {
			err = server.Serve()
		}
		if err != nil && err != io.EOF {
			fmt.Fprintln(os.Stderr, err)
			return 1
		}
		return 0
	}
	cmd := exec.Command("sh", "-c", args[len(args)-1])
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
	if err := cmd.Run(); err != nil {
		if exit, ok := err.(*exec.ExitError); ok {
			return exit.ExitCode()
		}
		return 255
	}
	return 0
}

// sftpRemote is an sftp fs string for dir through the fake ssh, which logs in to home.
func sftpRemote(t *testing.T, home, dir string) string {
	exe, err := os.Executable()
	require.NoError(t, err)
	t.Setenv("RSE_FAKE_SSH", "1")
	t.Setenv("RSE_FAKE_SSH_HOME", home)
	return fmt.Sprintf(":sftp,ssh='%s':%s", exe, dir)
}

var groups int

// call runs an rc method as a job of its own and returns the bytes it transferred.
func call(t *testing.T, method string, in rc.Params) (int64, error) {
	t.Helper()
	groups++
	group := fmt.Sprintf("test/%d", groups)
	in["_group"] = group
	_, _, err := jobs.NewJob(context.Background(), rc.Calls.Get(method).Fn, in)
	return accounting.StatsGroup(context.Background(), group).GetBytes(), err
}

func randomFile(t *testing.T, name string, size int) []byte {
	t.Helper()
	data := make([]byte, size)
	_, _ = rand.Read(data)
	require.NoError(t, os.MkdirAll(filepath.Dir(name), 0o755))
	require.NoError(t, os.WriteFile(name, data, 0o644))
	return data
}

func copyParams(srcFs, srcRemote, dstFs, dstRemote string) rc.Params {
	return rc.Params{"srcFs": srcFs, "srcRemote": srcRemote, "dstFs": dstFs, "dstRemote": dstRemote}
}

func assertFile(t *testing.T, name string, want []byte) {
	t.Helper()
	got, err := os.ReadFile(name)
	require.NoError(t, err)
	assert.True(t, bytes.Equal(got, want), "%s differs", name)
	assert.NoFileExists(t, name+PartialSuffix)
}

const mib = 1024 * 1024

func TestLocalResumesAPrefix(t *testing.T) {
	src, dst := t.TempDir(), t.TempDir()
	data := randomFile(t, filepath.Join(src, "big"), 5*mib)
	require.NoError(t, os.WriteFile(filepath.Join(dst, "big"+PartialSuffix), data[:3*mib], 0o644))

	n, err := call(t, "rse/copy", copyParams(src, "big", dst, "big"))
	require.NoError(t, err)
	assert.Equal(t, int64(2*mib), n)
	assertFile(t, filepath.Join(dst, "big"), data)
}

func TestLocalStartsOverWhenThePartialDiffers(t *testing.T) {
	src, dst := t.TempDir(), t.TempDir()
	data := randomFile(t, filepath.Join(src, "big"), 3*mib)
	randomFile(t, filepath.Join(dst, "big"+PartialSuffix), 2*mib)

	n, err := call(t, "rse/copy", copyParams(src, "big", dst, "big"))
	require.NoError(t, err)
	assert.Equal(t, int64(3*mib), n)
	assertFile(t, filepath.Join(dst, "big"), data)

	// Longer than the source: also started over.
	randomFile(t, filepath.Join(src, "small"), 1000)
	big := append(bytes.Clone(data), 1)
	require.NoError(t, os.WriteFile(filepath.Join(dst, "small"+PartialSuffix), big, 0o644))
	n, err = call(t, "rse/copy", copyParams(src, "small", dst, ""))
	require.NoError(t, err)
	assert.Equal(t, int64(1000), n)
}

// A partial file whose end matches but whose middle does not fails the check of the
// finished file, which is then copied again from the start.
func TestCorruptPartialIsCopiedAgain(t *testing.T) {
	src, home := t.TempDir(), t.TempDir()
	data := randomFile(t, filepath.Join(src, "big"), 5*mib)
	bad := bytes.Clone(data[:4*mib])
	bad[100] ^= 1
	for name, dst := range map[string]string{"local": home, "sftp": sftpRemote(t, home, home)} {
		require.NoError(t, os.WriteFile(filepath.Join(home, "big"+PartialSuffix), bad, 0o644))
		n, err := call(t, "rse/copy", copyParams(src, "big", dst, "big"))
		require.NoError(t, err, name)
		assert.Greater(t, n, int64(len(data)), name)
		assertFile(t, filepath.Join(home, "big"), data)
		require.NoError(t, os.Remove(filepath.Join(home, "big")))
	}
}

func TestIdenticalFilesAreSkipped(t *testing.T) {
	src, dst := t.TempDir(), t.TempDir()
	data := randomFile(t, filepath.Join(src, "f"), 100*1024)
	n, err := call(t, "rse/copy", copyParams(src, "f", dst, "f"))
	require.NoError(t, err)
	assert.Equal(t, int64(len(data)), n)

	n, err = call(t, "rse/copy", copyParams(src, "f", dst, "f"))
	require.NoError(t, err)
	assert.Zero(t, n, "same size and time")

	// Same size and time, other content: only a checksum tells.
	stat, err := os.Stat(filepath.Join(dst, "f"))
	require.NoError(t, err)
	randomFile(t, filepath.Join(dst, "f"), len(data))
	require.NoError(t, os.Chtimes(filepath.Join(dst, "f"), stat.ModTime(), stat.ModTime()))
	n, err = call(t, "rse/copy", copyParams(src, "f", dst, "f"))
	require.NoError(t, err)
	assert.Zero(t, n)
	in := copyParams(src, "f", dst, "f")
	in["checksum"] = true
	n, err = call(t, "rse/copy", in)
	require.NoError(t, err)
	assert.Equal(t, int64(len(data)), n)
	assertFile(t, filepath.Join(dst, "f"), data)
}

func setBwlimit(t *testing.T, rate string) {
	t.Helper()
	_, err := rc.Calls.Get("core/bwlimit").Fn(context.Background(), rc.Params{"rate": rate})
	require.NoError(t, err)
}

func TestStoppedJobLeavesThePartial(t *testing.T) {
	src, dst := t.TempDir(), t.TempDir()
	data := randomFile(t, filepath.Join(src, "big"), 4*mib)
	setBwlimit(t, "1M")
	t.Cleanup(func() { setBwlimit(t, "off") })
	in := copyParams(src, "big", dst, "big")
	in["_async"] = true
	job, _, err := jobs.NewJob(context.Background(), rc.Calls.Get("rse/copy").Fn, in)
	require.NoError(t, err)
	time.Sleep(1500 * time.Millisecond)
	stats, err := rc.Calls.Get("core/stats").Fn(context.Background(), rc.Params{"group": job.Group})
	require.NoError(t, err)
	assert.Positive(t, stats["bytes"])
	assert.Equal(t, int64(len(data)), stats["totalBytes"])
	assert.Len(t, stats["transferring"], 1)
	job.Stop()
	var status rc.Params
	require.Eventually(t, func() bool {
		status, err = rc.Calls.Get("job/status").Fn(context.Background(), rc.Params{"jobid": job.ID})
		return err == nil && status["finished"] == true
	}, 10*time.Second, 20*time.Millisecond)
	assert.Equal(t, false, status["success"])
	part, err := os.ReadFile(filepath.Join(dst, "big"+PartialSuffix))
	require.NoError(t, err)
	assert.NotEmpty(t, part)
	assert.Less(t, len(part), len(data))
	assert.True(t, bytes.Equal(part, data[:len(part)]))
	assert.NoFileExists(t, filepath.Join(dst, "big"))

	setBwlimit(t, "off")
	n, err := call(t, "rse/copy", copyParams(src, "big", dst, "big"))
	require.NoError(t, err)
	assert.Equal(t, int64(len(data)-len(part)), n)
	assertFile(t, filepath.Join(dst, "big"), data)
}

func TestUploadToSftpResumes(t *testing.T) {
	src, home := t.TempDir(), t.TempDir()
	// Relative to the login directory, as rclone's sftp backend has it.
	remote := sftpRemote(t, home, "up")
	data := randomFile(t, filepath.Join(src, "big"), 5*mib)
	require.NoError(t, os.MkdirAll(filepath.Join(home, "up", "in"), 0o755))
	require.NoError(t, os.WriteFile(filepath.Join(home, "up", "in", "big"+PartialSuffix), data[:4*mib], 0o644))

	n, err := call(t, "rse/copy", copyParams(src, "big", remote, "in/big"))
	require.NoError(t, err)
	// Resumed 2 MiB before the end of the partial file: the concurrent writes' slack.
	assert.Equal(t, int64(3*mib), n)
	assertFile(t, filepath.Join(home, "up", "in", "big"), data)

	// A new file, into a new directory.
	n, err = call(t, "rse/copy", copyParams(src, "big", remote, "new/dir/big"))
	require.NoError(t, err)
	assert.Equal(t, int64(len(data)), n)
	assertFile(t, filepath.Join(home, "up", "new", "dir", "big"), data)
}

func TestDownloadFromSftpResumes(t *testing.T) {
	home, dst := t.TempDir(), t.TempDir()
	data := randomFile(t, filepath.Join(home, "srv", "big"), 3*mib)
	require.NoError(t, os.WriteFile(filepath.Join(dst, "big"+PartialSuffix), data[:mib], 0o644))

	n, err := call(t, "rse/copy", copyParams(sftpRemote(t, home, filepath.Join(home, "srv")), "big", dst, "big"))
	require.NoError(t, err)
	assert.Equal(t, int64(2*mib), n)
	assertFile(t, filepath.Join(dst, "big"), data)
}

func TestOtherTargetsCopyWithRclone(t *testing.T) {
	src := t.TempDir()
	randomFile(t, filepath.Join(src, "f"), 1000)
	n, err := call(t, "rse/copy", copyParams(src, "f", ":memory:bucket", "f"))
	require.NoError(t, err)
	assert.Equal(t, int64(1000), n)
}

func TestDirectories(t *testing.T) {
	src, home := t.TempDir(), t.TempDir()
	a := randomFile(t, filepath.Join(src, "d", "a"), 1000)
	b := randomFile(t, filepath.Join(src, "d", "sub", "b"), 2000)
	randomFile(t, filepath.Join(src, "d", "sub", "c"+PartialSuffix), 10)
	require.NoError(t, os.MkdirAll(filepath.Join(src, "d", "empty"), 0o755))

	// Copy to the local disk, with empty directories.
	dst := t.TempDir()
	in := rc.Params{"srcFs": filepath.Join(src, "d"), "dstFs": filepath.Join(dst, "d"), "isDir": true, "createEmptySrcDirs": true}
	n, err := call(t, "rse/copy", in)
	require.NoError(t, err)
	assert.Equal(t, int64(3000), n)
	assertFile(t, filepath.Join(dst, "d", "a"), a)
	assertFile(t, filepath.Join(dst, "d", "sub", "b"), b)
	assert.NoFileExists(t, filepath.Join(dst, "d", "sub", "c"+PartialSuffix), "partial files are not copied")
	assert.DirExists(t, filepath.Join(dst, "d", "empty"))

	// Move to sftp, addressed by a remote within the fs: the emptied directories go,
	// all but the partial file's and the root.
	remote := sftpRemote(t, home, home)
	in = rc.Params{"srcFs": src, "srcRemote": "d", "dstFs": remote, "dstRemote": "moved", "isDir": true, "move": true}
	n, err = call(t, "rse/copy", in)
	require.NoError(t, err)
	assert.Equal(t, int64(3000), n)
	assertFile(t, filepath.Join(home, "moved", "a"), a)
	assertFile(t, filepath.Join(home, "moved", "sub", "b"), b)
	assert.NoDirExists(t, filepath.Join(home, "moved", "empty"))
	assert.NoFileExists(t, filepath.Join(src, "d", "a"))
	assert.NoDirExists(t, filepath.Join(src, "d", "empty"))
	assert.FileExists(t, filepath.Join(src, "d", "sub", "c"+PartialSuffix))
}

func TestMoveWithinTheLocalDisk(t *testing.T) {
	src, dst := t.TempDir(), t.TempDir()
	data := randomFile(t, filepath.Join(src, "f"), 1000)
	in := copyParams(src, "f", dst, "g")
	in["move"] = true
	_, err := call(t, "rse/copy", in)
	require.NoError(t, err)
	assertFile(t, filepath.Join(dst, "g"), data)
	assert.NoFileExists(t, filepath.Join(src, "f"))
}

func TestResumable(t *testing.T) {
	for dst, want := range map[string]bool{
		"/tmp":                       true,
		":local:/tmp":                true,
		":sftp,ssh='ssh box':/srv":   true,
		":sftp,host=box:/srv":        false,
		":memory:bucket":             false,
		":sftp,ssh='',host=box:/srv": false,
		":crypt,remote='/tmp':":      false,
	} {
		out, err := rc.Calls.Get("rse/resumable").Fn(context.Background(), rc.Params{"srcFs": "/", "dstFs": dst})
		require.NoError(t, err, dst)
		assert.Equal(t, want, out["resumable"], dst)
	}
	_, err := rc.Calls.Get("rse/resumable").Fn(context.Background(), rc.Params{"srcFs": "/", "dstFs": "nosuchremote:"})
	assert.Error(t, err)
}

// The fs strings rclone makes for remotes with parameters of their own name the same
// config as the strings they came from.
func TestTargetForParsedFs(t *testing.T) {
	f, err := fs.NewFs(context.Background(), ":local,encoding=Slash:"+t.TempDir())
	require.NoError(t, err)
	tgt, err := targetFor(f)
	require.NoError(t, err)
	assert.IsType(t, &localTarget{}, tgt)
}
