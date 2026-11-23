package resume

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/accounting"
	"github.com/rclone/rclone/fs/operations"
	"github.com/rclone/rclone/fs/rc"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// fakeChunks keeps the chunks written to it, failing chunk fail its first failures
// times, after reading half of it.
type fakeChunks struct {
	mu       sync.Mutex
	chunks   map[int][]byte
	fail     int
	failures int
	tries    map[int]int
	closed   bool
	aborted  bool
}

func (f *fakeChunks) WriteChunk(_ context.Context, chunk int, r io.ReadSeeker) (int64, error) {
	f.mu.Lock()
	f.tries[chunk]++
	failing := chunk == f.fail && f.tries[chunk] <= f.failures
	f.mu.Unlock()
	data, err := io.ReadAll(r)
	if err != nil {
		return -1, err
	}
	if failing {
		// Read again from the start on the next try, as a backend does.
		_, _ = r.Seek(0, io.SeekStart)
		return -1, errors.New("connection reset")
	}
	f.mu.Lock()
	f.chunks[chunk] = data
	f.mu.Unlock()
	return int64(len(data)), nil
}

func (f *fakeChunks) Close(context.Context) error { f.closed = true; return nil }
func (f *fakeChunks) Abort(context.Context) error { f.aborted = true; return nil }

func (f *fakeChunks) joined(n int) []byte {
	var out []byte
	for i := range n {
		out = append(out, f.chunks[i]...)
	}
	return out
}

// memoryObject puts data at name in a memory remote of its own.
func memoryObject(t *testing.T, name string, data []byte) fs.Object {
	t.Helper()
	ctx := context.Background()
	f, err := fs.NewFs(ctx, fmt.Sprintf(":memory:%s", t.Name()))
	require.NoError(t, err)
	o, err := operations.Rcat(ctx, f, name, io.NopCloser(bytes.NewReader(data)), time.Now(), nil)
	require.NoError(t, err)
	return o
}

// A chunk that fails is sent again by itself, up to the number of retries, and the
// bytes are counted once.
func TestAFailedChunkIsSentAgainByItself(t *testing.T) {
	data := make([]byte, 10*1024)
	for i := range data {
		data[i] = byte(i * 7)
	}
	src := memoryObject(t, "f", data)
	ctx := accounting.WithStatsGroup(context.Background(), "test/chunks")
	tr := accounting.Stats(ctx).NewTransfer(src, nil)
	acc := tr.Account(ctx, nil)

	w := &fakeChunks{chunks: map[int][]byte{}, tries: map[int]int{}, fail: 1, failures: 2}
	c := &chunking{retries: 2, delay: time.Millisecond, backoff: true}
	require.NoError(t, c.upload(ctx, src, w, 4096, 2, acc))
	assert.Equal(t, map[int]int{0: 1, 1: 3, 2: 1}, w.tries)
	assert.True(t, bytes.Equal(data, w.joined(3)))
	assert.Equal(t, int64(len(data)), accounting.StatsGroup(ctx, "test/chunks").GetBytes())
	tr.Done(ctx, nil)

	// One retry fewer than it needs: the job fails, naming the chunk.
	w = &fakeChunks{chunks: map[int][]byte{}, tries: map[int]int{}, fail: 1, failures: 2}
	c.retries = 1
	err := c.upload(context.Background(), src, w, 4096, 1, acc)
	assert.ErrorContains(t, err, "chunk 2 of 3 failed after 2 tries: connection reset")
	assert.Equal(t, 2, w.tries[1])
	assert.Zero(t, w.tries[2], "nothing more is sent once a chunk has failed")
}

func TestRetriesWaitTwiceAsLongEachTime(t *testing.T) {
	c := &chunking{delay: time.Second, backoff: true}
	assert.Equal(t, []time.Duration{time.Second, 2 * time.Second, 4 * time.Second},
		[]time.Duration{c.wait(1), c.wait(2), c.wait(3)})
	assert.Equal(t, maxChunkDelay, c.wait(20))
	c.backoff = false
	assert.Equal(t, time.Second, c.wait(5))
}

// A target that writes at offsets takes the file in chunks; what it has is the file,
// with the source's modification time. A move removes the source.
func TestChunkedCopyToATargetThatWritesAtOffsets(t *testing.T) {
	ctx := context.Background()
	data := make([]byte, 300*1024)
	for i := range data {
		data[i] = byte(i % 251)
	}
	src := memoryObject(t, "big", data)
	fsrc, err := fs.NewFs(ctx, ":memory:"+t.Name())
	require.NoError(t, err)
	dir := t.TempDir()
	fdst, err := fs.NewFs(ctx, dir)
	require.NoError(t, err)
	j := &job{fsrc: fsrc, fdst: fdst, move: true, chunks: &chunking{size: 64 * 1024, delay: time.Millisecond}}
	ctx = accounting.WithStatsGroup(ctx, "test/chunked-copy")
	require.NoError(t, j.file(ctx, "sub/big", src))
	got, err := os.ReadFile(filepath.Join(dir, "sub", "big"))
	require.NoError(t, err)
	assert.True(t, bytes.Equal(data, got))
	stat, err := os.Stat(filepath.Join(dir, "sub", "big"))
	require.NoError(t, err)
	assert.WithinDuration(t, src.ModTime(ctx), stat.ModTime(), time.Second)
	assert.Equal(t, int64(len(data)), accounting.StatsGroup(ctx, "test/chunked-copy").GetBytes())
	_, err = fsrc.NewObject(ctx, "big")
	assert.ErrorIs(t, err, fs.ErrorObjectNotFound)
}

// Files no larger than a chunk, targets that resume partial files, and targets that
// cannot take chunks are copied as without chunked.
func TestChunkedFallsBackWhereChunksDoNotApply(t *testing.T) {
	src := t.TempDir()
	data := randomFile(t, filepath.Join(src, "f"), 100*1024)
	for _, dst := range []string{t.TempDir(), ":memory:chunked"} {
		in := copyParams(src, "f", dst, "f")
		in["chunked"] = true
		in["chunkSize"] = 8 * 1024
		n, err := call(t, "rse/copy", in)
		require.NoError(t, err, dst)
		assert.Equal(t, int64(len(data)), n, dst)
	}
	in := copyParams(src, "f", ":memory:chunked", "g")
	in["chunked"] = true
	in["chunkSize"] = 0
	_, err := call(t, "rse/copy", in)
	assert.Error(t, err, "a chunk size of nothing")
}

func TestChunkingParams(t *testing.T) {
	c, err := chunkingParams(rc.Params{})
	require.NoError(t, err)
	assert.Nil(t, c)
	c, err = chunkingParams(rc.Params{"chunked": true})
	require.NoError(t, err)
	assert.Equal(t, &chunking{size: 64 << 20, delay: 2 * time.Second, backoff: true}, c)
	c, err = chunkingParams(rc.Params{"chunked": true, "chunkSize": 1024, "chunkRetries": 5, "chunkDelayMs": 500, "chunkBackoff": false})
	require.NoError(t, err)
	assert.Equal(t, &chunking{size: 1024, retries: 5, delay: 500 * time.Millisecond}, c)
}
