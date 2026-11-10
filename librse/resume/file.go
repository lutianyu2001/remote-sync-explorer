package resume

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"path"
	"time"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/accounting"
	"github.com/rclone/rclone/fs/fserrors"
	"github.com/rclone/rclone/fs/hash"
	"github.com/rclone/rclone/fs/operations"
)

const (
	// PartialSuffix ends the name a file is written under until it is complete. rsync,
	// rse's other engine, keeps its partial files in a directory of this name.
	PartialSuffix = ".rse-partial"
	// tailSize is how much of a partial file's end is compared with the source before
	// resuming it.
	tailSize = 64 * 1024
	// maxName is the longest file name most file systems take.
	maxName = 255
)

// job is one rse/copy call.
type job struct {
	fsrc, fdst fs.Fs
	// target writes partial files on fdst; nil when it cannot.
	target             target
	move               bool
	createEmptySrcDirs bool
}

// file copies or moves src to remote on fdst, unless the file there is the same.
func (j *job) file(ctx context.Context, remote string, src fs.Object) error {
	dst, err := j.fdst.NewObject(ctx, remote)
	if errors.Is(err, fs.ErrorObjectNotFound) {
		dst = nil
	} else if err != nil {
		return err
	}
	if dst != nil && operations.SameObject(src, dst) {
		fs.Debugf(src, "already at its target")
		return nil
	}
	if dst != nil && !operations.NeedTransfer(ctx, dst, src) {
		if j.move {
			return operations.DeleteFile(ctx, src)
		}
		return nil
	}
	// An empty file has nothing to resume, and writing it would leave no partial file.
	if j.target == nil || src.Size() <= 0 || len(path.Base(remote))+len(PartialSuffix) > maxName {
		if j.move {
			_, err = operations.Move(ctx, j.fdst, dst, remote, src)
		} else {
			_, err = operations.Copy(ctx, j.fdst, dst, remote, src)
		}
		return err
	}
	sameConfig := operations.SameConfig(src.Fs(), j.fdst)
	if j.move && sameConfig && j.fdst.Features().Move != nil {
		if moved, err := j.serverSideMove(ctx, remote, src); moved || err != nil {
			return err
		}
	}
	if err := j.resumableCopy(ctx, remote, src, sameConfig); err != nil {
		return err
	}
	if j.move {
		return operations.DeleteFile(ctx, src)
	}
	return nil
}

// serverSideMove moves src within its own fs. It returns false, and no error, when
// the fs cannot, such as across disks.
func (j *job) serverSideMove(ctx context.Context, remote string, src fs.Object) (moved bool, err error) {
	tr := accounting.Stats(ctx).NewCheckingTransfer(src, "moving")
	defer func() { tr.Done(ctx, err) }()
	in := tr.Account(ctx, nil)
	in.ServerSideTransferStart()
	defer func() { _ = in.Close() }()
	newDst, err := j.fdst.Features().Move(ctx, src, remote)
	if errors.Is(err, fs.ErrorCantMove) {
		fs.Debugf(src, "cannot move server-side, copying")
		return false, nil
	}
	if err != nil {
		return false, err
	}
	in.ServerSideMoveEnd(newDst.Size())
	accounting.Stats(ctx).Renames(1)
	fs.Infof(src, "Moved (server-side) to: %s", newDst.String())
	return true, nil
}

// resumableCopy copies src to remote on fdst through remote+PartialSuffix, which it
// resumes when it is a prefix of src and leaves in place when stopped or failing.
func (j *job) resumableCopy(ctx context.Context, remote string, src fs.Object, sameConfig bool) (err error) {
	part := remote + PartialSuffix
	size := src.Size()
	partial, err := j.fdst.NewObject(ctx, part)
	switch {
	case errors.Is(err, fs.ErrorObjectNotFound):
		partial = nil
	case errors.Is(err, fs.ErrorIsDir) || errors.Is(err, fs.ErrorNotAFile):
		return fmt.Errorf("refusing to write %q: not a regular file: %w", part, err)
	case err != nil:
		return err
	}
	var off int64
	if partial != nil {
		if off, err = j.resumeAt(ctx, partial, src, false); err != nil {
			return err
		}
	}
	resumedAt := off
	// A partial file this job wrote and leaves, stopped or failing, is marked as a copy
	// of this source.
	wrote := false
	defer func() {
		if err != nil && wrote {
			j.markPartial(ctx, part, src)
		}
	}()

	tr := accounting.Stats(ctx).NewTransferRemoteSize(src.Remote(), size-off, j.fsrc, j.fdst)
	defer func() { tr.Done(ctx, err) }()

	written := false
	if partial == nil && sameConfig && j.fdst.Features().Copy != nil {
		// Such as a clone on the same disk: nothing to resume if it can.
		err = serverSideCopy(ctx, tr, j.fdst, part, src)
		if err == nil {
			written = true
		} else if !errors.Is(err, fs.ErrorCantCopy) {
			return err
		}
	}
	for retried := false; ; retried = true {
		if !written {
			wrote = true
			if err = j.write(ctx, tr, part, src, off); err != nil {
				return err
			}
		}
		written = false
		if partial, err = j.fdst.NewObject(ctx, part); err != nil {
			return err
		}
		var why error
		if partial.Size() != size {
			why = fmt.Errorf("sizes differ src %d vs partial %d", size, partial.Size())
		} else {
			equal, ht, err := operations.CheckHashes(ctx, src, partial)
			if err != nil {
				return err
			}
			if !equal {
				why = fmt.Errorf("%v hashes differ", ht)
			} else if ht == hash.None && resumedAt > 0 {
				fs.Infof(src, "Resumed at %d bytes, unverified: no hash in common with %v", resumedAt, j.fdst)
			}
		}
		if why == nil {
			break
		}
		if retried {
			_ = partial.Remove(ctx)
			return fmt.Errorf("corrupted on transfer: %w", why)
		}
		fs.Errorf(src, "Copying again from the start: %v", why)
		off = 0
	}

	if err := partial.SetModTime(ctx, src.ModTime(ctx)); err != nil {
		fs.Debugf(partial, "cannot set modification time: %v", err)
	}
	doMove := j.fdst.Features().Move
	if doMove == nil {
		return fmt.Errorf("%v cannot rename %q", j.fdst, part)
	}
	if _, err = doMove(ctx, partial, remote); err != nil {
		return fmt.Errorf("renaming %q: %w", part, err)
	}
	fs.Infof(src, "Copied to: %s", remote)
	return nil
}

// resumeAt returns where to resume partial, a partial copy of src: back by the
// target's slack from its end, if what it has there is what src has. Otherwise 0, to
// start over. Unless this job wrote it (own), partial must have src's modification
// time, which markPartial gave it: one whose source has changed since, or which a
// crash left with the time of its last write, is not resumed.
func (j *job) resumeAt(ctx context.Context, partial, src fs.Object, own bool) (int64, error) {
	have := partial.Size()
	if have > src.Size() {
		fs.Infof(src, "Starting over: %s is larger than the source", partial)
		return 0, nil
	}
	if !own && !sameTime(partial.ModTime(ctx), src.ModTime(ctx), j.fdst.Precision()) {
		fs.Infof(src, "Starting over: %s was not left by a copy of this source", partial)
		return 0, nil
	}
	off := have - j.target.slack()
	if off <= 0 {
		return 0, nil
	}
	n := min(int64(tailSize), off)
	ours, err := readRange(ctx, partial, off-n, n)
	if err != nil {
		fs.Infof(src, "Starting over: %v", err)
		return 0, nil
	}
	theirs, err := readRange(ctx, src, off-n, n)
	if err != nil {
		return 0, err
	}
	if !bytes.Equal(ours, theirs) {
		fs.Infof(src, "Starting over: %s differs from the source", partial)
		return 0, nil
	}
	fs.Infof(src, "Resuming at %d of %d bytes", off, src.Size())
	return off, nil
}

// sameTime says whether a and b are one time, as a file system of precision keeps it.
func sameTime(a, b time.Time, precision time.Duration) bool {
	if precision == fs.ModTimeNotSupported {
		return false
	}
	d := a.Sub(b)
	return d <= precision && -d <= precision
}

// markPartial gives a partial file the job leaves the modification time of its source,
// which resumeAt asks for, even when the job was stopped.
func (j *job) markPartial(ctx context.Context, part string, src fs.Object) {
	ctx = context.WithoutCancel(ctx)
	partial, err := j.fdst.NewObject(ctx, part)
	if err == nil {
		err = partial.SetModTime(ctx, src.ModTime(ctx))
	}
	if err != nil {
		fs.Debugf(src, "Cannot mark %s as its partial copy: %v", part, err)
	}
}

// readRange reads n bytes of o from off.
func readRange(ctx context.Context, o fs.Object, off, n int64) ([]byte, error) {
	in, err := operations.Open(ctx, o, &fs.RangeOption{Start: off, End: off + n - 1})
	if err != nil {
		return nil, err
	}
	defer func() { _ = in.Close() }()
	buf := make([]byte, n)
	if _, err := io.ReadFull(in, buf); err != nil {
		return nil, fmt.Errorf("reading %s: %w", o, err)
	}
	return buf, nil
}

// write copies src from off to the partial file part, from off, with low-level
// retries that resume where the last try got to.
func (j *job) write(ctx context.Context, tr *accounting.Transfer, part string, src fs.Object, off int64) (err error) {
	ci := fs.GetConfig(ctx)
	if dir := path.Dir(part); dir != "." {
		if err := j.fdst.Mkdir(ctx, dir); err != nil {
			return err
		}
	}
	for tries := 1; ; tries++ {
		err = j.writeOnce(ctx, tr, part, src, off)
		if fserrors.ContextError(ctx, &err) || err == nil || tries >= ci.LowLevelRetries || !fserrors.ShouldRetry(err) {
			return err
		}
		fs.Debugf(src, "Resuming after error, low level retry %d/%d: %v", tries, ci.LowLevelRetries, err)
		partial, perr := j.fdst.NewObject(ctx, part)
		if perr != nil {
			return err
		}
		if off, perr = j.resumeAt(ctx, partial, src, true); perr != nil {
			return err
		}
	}
}

func (j *job) writeOnce(ctx context.Context, tr *accounting.Transfer, part string, src fs.Object, off int64) error {
	want := src.Size() - off
	if want == 0 {
		return nil
	}
	var options []fs.OpenOption
	if off > 0 {
		options = append(options, &fs.RangeOption{Start: off, End: -1})
	}
	in, err := operations.Open(ctx, src, options...)
	if err != nil {
		return fmt.Errorf("failed to open source object: %w", err)
	}
	acc := tr.Account(ctx, in)
	defer func() { _ = acc.Close() }()
	out, err := j.target.open(ctx, part, off)
	if err != nil {
		return err
	}
	n, err := io.Copy(out, io.LimitReader(ctxReader{ctx, acc}, want))
	if cerr := out.Close(); err == nil {
		err = cerr
	}
	if err == nil && n < want {
		err = fmt.Errorf("source ended after %d of %d bytes", off+n, src.Size())
	}
	return err
}

// serverSideCopy copies src to remote within one fs, or returns fs.ErrorCantCopy.
func serverSideCopy(ctx context.Context, tr *accounting.Transfer, f fs.Fs, remote string, src fs.Object) error {
	in := tr.Account(ctx, nil)
	in.ServerSideTransferStart()
	newCtx, ta := in.NewServerSideCopyAccounter(ctx)
	newDst, err := f.Features().Copy(newCtx, src, remote)
	if err == nil {
		var n int64
		if !ta.Started() {
			n = newDst.Size()
		}
		in.ServerSideCopyEnd(n)
	} else {
		ta.Reset()
	}
	_ = in.Close()
	if errors.Is(err, fs.ErrorCantCopy) {
		tr.Reset(ctx)
	}
	return err
}

// ctxReader stops reading once ctx is done, which a stopped job's is.
type ctxReader struct {
	ctx context.Context
	r   io.Reader
}

func (c ctxReader) Read(p []byte) (int, error) {
	if err := c.ctx.Err(); err != nil {
		return 0, err
	}
	return c.r.Read(p)
}
