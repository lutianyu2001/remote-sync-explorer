package resume

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"sync"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/accounting"
	"github.com/rclone/rclone/fs/fserrors"
	"github.com/rclone/rclone/fs/hash"
	"github.com/rclone/rclone/fs/operations"
	"github.com/rclone/rclone/fs/rc"
)

const (
	// maxChunkDelay is the longest a chunk waits before it is sent again.
	maxChunkDelay = 5 * time.Minute
	// maxChunkStreams is how many chunks are under way at once at most, each held in
	// memory while it is sent.
	maxChunkStreams = 4
)

// chunking says how rse/copy sends files in chunks when asked to (chunked=true): each
// chunk on its own, sent again on its own when it fails, and the target puts the file
// together once all have arrived, as a multipart upload does on S3 and the like.
type chunking struct {
	// size is the chunk size asked for; the target may take another.
	size int64
	// retries is how many more times a failed chunk is sent.
	retries int
	// delay is how long the first retry of a chunk waits.
	delay time.Duration
	// backoff doubles the wait of each retry after the first.
	backoff bool
}

// chunkingParams reads the chunking parameters of rse/copy; nil when chunked is not
// set.
func chunkingParams(in rc.Params) (*chunking, error) {
	on, err := optionalBool(in, "chunked")
	if err != nil || !on {
		return nil, err
	}
	c := &chunking{size: 64 << 20, delay: 2 * time.Second, backoff: true}
	if n, err := in.GetInt64("chunkSize"); err == nil {
		if n <= 0 {
			return nil, rc.NewErrParamInvalid(errors.New("chunkSize must be positive"))
		}
		c.size = n
	} else if !rc.IsErrParamNotFound(err) {
		return nil, err
	}
	if n, err := in.GetInt64("chunkRetries"); err == nil {
		c.retries = int(max(n, 0))
	} else if !rc.IsErrParamNotFound(err) {
		return nil, err
	}
	if n, err := in.GetInt64("chunkDelayMs"); err == nil {
		c.delay = time.Duration(max(n, 0)) * time.Millisecond
	} else if !rc.IsErrParamNotFound(err) {
		return nil, err
	}
	if b, err := in.GetBool("chunkBackoff"); err == nil {
		c.backoff = b
	} else if !rc.IsErrParamNotFound(err) {
		return nil, err
	}
	return c, nil
}

// wait is how long retry number try (1 for the first) of a chunk waits.
func (c *chunking) wait(try int) time.Duration {
	d := c.delay
	for i := 1; c.backoff && i < try && d < maxChunkDelay; i++ {
		d *= 2
	}
	return min(d, maxChunkDelay)
}

// chunkable says whether f can put a file together from chunks sent on their own: a
// multipart upload, or writing each chunk at its place in the file.
func chunkable(f fs.Fs) bool {
	ft := f.Features()
	return ft.OpenChunkWriter != nil || ft.OpenWriterAt != nil
}

// chunkedCopy copies src to remote on fdst in chunks, each sent again on its own when
// it fails, as c says; the target puts them together at the end. A failure leaves
// nothing at remote: the chunks sent are given up.
func (j *job) chunkedCopy(ctx context.Context, remote string, src fs.Object) (err error) {
	c := j.chunks
	size := src.Size()
	tr := accounting.Stats(ctx).NewTransfer(src, j.fdst)
	defer func() { tr.Done(ctx, err) }()

	var info fs.ChunkWriterInfo
	var w fs.ChunkWriter
	atOffsets := false
	if open := j.fdst.Features().OpenChunkWriter; open != nil {
		info, w, err = open(ctx, remote, src, &fs.ChunkOption{ChunkSize: c.size})
	} else if open := j.fdst.Features().OpenWriterAt; open != nil {
		var wa fs.WriterAtCloser
		wa, err = open(ctx, remote, size)
		w = &writerAtChunks{w: wa, f: j.fdst, remote: remote, size: c.size}
		info = fs.ChunkWriterInfo{ChunkSize: c.size, Concurrency: maxChunkStreams}
		atOffsets = true
	} else {
		return fmt.Errorf("%v cannot take a file in chunks", j.fdst)
	}
	if err != nil {
		return fmt.Errorf("starting the chunked upload: %w", err)
	}
	chunkSize := info.ChunkSize
	if chunkSize <= 0 {
		chunkSize = c.size
	}
	if atOffsets {
		w.(*writerAtChunks).size = chunkSize
	}
	streams := min(max(info.Concurrency, 1), maxChunkStreams)
	acc := tr.Account(ctx, nil)
	defer func() { _ = acc.Close() }()
	if err = c.upload(ctx, src, w, chunkSize, streams, acc); err != nil {
		if !info.LeavePartsOnError {
			if aerr := w.Abort(context.WithoutCancel(ctx)); aerr != nil {
				fs.Debugf(src, "Giving up the chunks sent: %v", aerr)
			}
		}
		return err
	}
	// The target puts the chunks together.
	if err = w.Close(ctx); err != nil {
		return fmt.Errorf("putting the chunks together: %w", err)
	}
	obj, err := j.fdst.NewObject(ctx, remote)
	if err != nil {
		return fmt.Errorf("finding the file put together: %w", err)
	}
	if obj.Size() != size {
		_ = obj.Remove(ctx)
		return fmt.Errorf("corrupted on transfer: sizes differ src %d vs dst %d", size, obj.Size())
	}
	if equal, ht, err := operations.CheckHashes(ctx, src, obj); err != nil {
		return err
	} else if !equal {
		_ = obj.Remove(ctx)
		return fmt.Errorf("corrupted on transfer: %v hashes differ", ht)
	} else if ht == hash.None {
		fs.Debugf(src, "Chunked copy unverified: no hash in common with %v", j.fdst)
	}
	if atOffsets {
		if err := obj.SetModTime(ctx, src.ModTime(ctx)); err != nil {
			fs.Debugf(obj, "cannot set modification time: %v", err)
		}
	}
	fs.Infof(src, "Copied in chunks of %v to: %s", fs.SizeSuffix(chunkSize), remote)
	return nil
}

// upload sends src to w in chunks of chunkSize, streams at once, each sent again on its
// own as c says, counting the bytes sent in acc once each.
func (c *chunking) upload(ctx context.Context, src fs.Object, w fs.ChunkWriter, chunkSize int64, streams int, acc *accounting.Account) error {
	size := src.Size()
	n := int((size + chunkSize - 1) / chunkSize)
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	todo := make(chan int)
	var mu sync.Mutex
	var first error
	var wg sync.WaitGroup
	for range min(streams, max(n, 1)) {
		wg.Go(func() {
			for chunk := range todo {
				start := int64(chunk) * chunkSize
				end := min(start+chunkSize, size)
				if err := c.chunk(ctx, src, w, chunk, n, start, end, acc); err != nil {
					mu.Lock()
					if first == nil {
						first = err
					}
					mu.Unlock()
					cancel()
				}
			}
		})
	}
feed:
	for chunk := range n {
		select {
		case todo <- chunk:
		case <-ctx.Done():
			break feed
		}
	}
	close(todo)
	wg.Wait()
	if first != nil {
		return first
	}
	return ctx.Err()
}

// chunk sends chunk number chunk of n, the bytes of src from start to end, trying again
// after each failure that may pass, as many times as c says.
func (c *chunking) chunk(ctx context.Context, src fs.Object, w fs.ChunkWriter, chunk, n int, start, end int64, acc *accounting.Account) error {
	var buf []byte
	counted := new(int64)
	for try := 0; ; try++ {
		if try > 0 {
			select {
			case <-time.After(c.wait(try)):
			case <-ctx.Done():
				return ctx.Err()
			}
		}
		var err error
		if buf == nil {
			buf, err = readRange(ctx, src, start, end-start)
		}
		if err == nil {
			r := &countingReader{r: bytes.NewReader(buf), counted: counted, acc: acc}
			_, err = w.WriteChunk(ctx, chunk, r)
		}
		if err == nil {
			// What a retry that read less left uncounted.
			if rest := int64(len(buf)) - *counted; rest > 0 {
				_ = acc.AccountRead(int(rest))
			}
			return nil
		}
		if fserrors.ContextError(ctx, &err) || fserrors.IsFatalError(err) || fserrors.IsNoRetryError(err) || try >= c.retries {
			if try > 0 {
				return fmt.Errorf("chunk %d of %d failed after %d tries: %w", chunk+1, n, try+1, err)
			}
			return fmt.Errorf("chunk %d of %d failed: %w", chunk+1, n, err)
		}
		fs.Infof(src, "Chunk %d of %d failed, sending it again in %v (%d of %d): %v", chunk+1, n, c.wait(try+1), try+1, c.retries, err)
	}
}

// countingReader counts in acc the bytes read past the furthest any read of the same
// chunk got before, so that a chunk sent again is counted once.
type countingReader struct {
	r       *bytes.Reader
	counted *int64
	acc     *accounting.Account
}

func (r *countingReader) Read(p []byte) (int, error) {
	n, err := r.r.Read(p)
	if at := r.r.Size() - int64(r.r.Len()); at > *r.counted {
		_ = r.acc.AccountRead(int(at - *r.counted))
		*r.counted = at
	}
	return n, err
}

func (r *countingReader) Seek(offset int64, whence int) (int64, error) {
	return r.r.Seek(offset, whence)
}

// writerAtChunks writes each chunk at its place in a file opened with OpenWriterAt.
type writerAtChunks struct {
	w      fs.WriterAtCloser
	f      fs.Fs
	remote string
	size   int64
	closed bool
}

func (w *writerAtChunks) WriteChunk(_ context.Context, chunk int, r io.ReadSeeker) (int64, error) {
	if _, err := r.Seek(0, io.SeekStart); err != nil {
		return -1, err
	}
	return io.Copy(io.NewOffsetWriter(w.w, int64(chunk)*w.size), r)
}

func (w *writerAtChunks) Close(context.Context) error {
	if w.closed {
		return nil
	}
	w.closed = true
	return w.w.Close()
}

func (w *writerAtChunks) Abort(ctx context.Context) error {
	_ = w.Close(ctx)
	obj, err := w.f.NewObject(ctx, w.remote)
	if err != nil {
		return err
	}
	return obj.Remove(ctx)
}
