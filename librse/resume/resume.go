// Package resume registers the rc calls rse/copy and rse/resumable: copies and moves
// that resume an interrupted file where it stopped, as rsync --partial does, instead of
// starting it over as rclone does.
//
// A file is written to its name plus PartialSuffix, next to its target, and renamed
// once complete and checked. A partial file that is left, by a stopped job or a failed
// one, is resumed by the next copy of the same file if its end matches the source.
// This needs a target that can be appended to, which rclone's backends cannot do:
// local disks, and sftp servers reached through an ssh command, which this package
// runs itself for an sftp session of its own. Anything else is copied by rclone.
package resume

import (
	"context"
	"errors"
	"fmt"
	"path"
	"slices"
	"strings"
	"sync"

	"github.com/rclone/rclone/fs"
	"github.com/rclone/rclone/fs/accounting"
	"github.com/rclone/rclone/fs/filter"
	"github.com/rclone/rclone/fs/operations"
	"github.com/rclone/rclone/fs/rc"
	"github.com/rclone/rclone/fs/walk"
)

func init() {
	rc.Add(rc.Call{
		Path:  "rse/copy",
		Fn:    rcCopy,
		Title: "Copy or move a file or a directory, resuming interrupted files",
		Help: `This takes the following parameters:

- srcFs - a remote name string e.g. "drive:" for the source
- srcRemote - a path within that remote e.g. "file.txt"; "" for the root of a directory
- dstFs - a remote name string e.g. "drive2:" for the destination
- dstRemote - a path within that remote; for a file, "" takes srcRemote
- isDir - true to copy the directory srcRemote and everything in it
- move - true to move instead of copy, deleting the emptied source directories
- checksum - true to compare files by checksum rather than size and modification time
- createEmptySrcDirs - true to create empty source directories on the destination

Files that are already the same on the destination are skipped. Files are written to
their name plus ".rse-partial" and renamed when complete; a partial file left by a
stopped or failed job is resumed. Run with _async=true and stop with job/stop as any
other job; core/stats for its group reports its progress.
`,
	})
	rc.Add(rc.Call{
		Path:  "rse/resumable",
		Fn:    rcResumable,
		Title: "Whether rse/copy resumes interrupted files from srcFs to dstFs",
		Help: `This takes the following parameters:

- srcFs - a remote name string e.g. "drive:" for the source
- dstFs - a remote name string for the destination

Returns:

- resumable - true when rse/copy writes partial files on dstFs that it can resume
`,
	})
}

// fsString is the fs string of parameter name: as given, or for a config given as an
// object, the one rclone makes of it.
func fsString(ctx context.Context, in rc.Params, name string) (string, error) {
	if s, err := in.GetString(name); err == nil {
		return s, nil
	}
	f, err := rc.GetFsNamed(ctx, in, name)
	if err != nil {
		return "", err
	}
	return fs.ConfigStringFull(f), nil
}

// optionalBool reads a boolean parameter that defaults to false.
func optionalBool(in rc.Params, name string) (bool, error) {
	b, err := in.GetBool(name)
	if rc.IsErrParamNotFound(err) {
		return false, nil
	}
	return b, err
}

// optionalString reads a string parameter that defaults to "".
func optionalString(in rc.Params, name string) (string, error) {
	s, err := in.GetString(name)
	if rc.IsErrParamNotFound(err) {
		return "", nil
	}
	return s, err
}

func rcResumable(ctx context.Context, in rc.Params) (rc.Params, error) {
	if _, err := fsString(ctx, in, "srcFs"); err != nil {
		return nil, err
	}
	dst, err := fsString(ctx, in, "dstFs")
	if err != nil {
		return nil, err
	}
	info, m, err := configOf(dst)
	if err != nil {
		return nil, err
	}
	t, err := newTarget(info, m, "")
	if err != nil {
		return nil, err
	}
	return rc.Params{"resumable": t != nil}, nil
}

func rcCopy(ctx context.Context, in rc.Params) (rc.Params, error) {
	var j job
	var err error
	if j.fsrc, err = rc.GetFsNamed(ctx, in, "srcFs"); err != nil {
		return nil, err
	}
	if j.fdst, err = rc.GetFsNamed(ctx, in, "dstFs"); err != nil {
		return nil, err
	}
	srcRemote, err := optionalString(in, "srcRemote")
	if err != nil {
		return nil, err
	}
	dstRemote, err := optionalString(in, "dstRemote")
	if err != nil {
		return nil, err
	}
	var isDir, checksum bool
	for name, p := range map[string]*bool{
		"isDir":              &isDir,
		"move":               &j.move,
		"checksum":           &checksum,
		"createEmptySrcDirs": &j.createEmptySrcDirs,
	} {
		if *p, err = optionalBool(in, name); err != nil {
			return nil, err
		}
	}
	if j.target, err = targetFor(j.fdst); err != nil {
		return nil, err
	}
	if j.target != nil {
		defer j.target.close()
	}
	if checksum {
		var ci *fs.ConfigInfo
		ctx, ci = fs.AddConfig(ctx)
		ci.CheckSum = true
	}
	if isDir {
		return nil, j.dir(ctx, srcRemote, dstRemote)
	}
	if srcRemote == "" {
		return nil, rc.NewErrParamInvalid(errors.New("srcRemote names no file"))
	}
	if dstRemote == "" {
		dstRemote = srcRemote
	}
	src, err := j.fsrc.NewObject(ctx, srcRemote)
	if err != nil {
		return nil, err
	}
	return nil, j.file(ctx, dstRemote, src)
}

// dir copies or moves the directory srcDir of fsrc to dstDir of fdst.
func (j *job) dir(ctx context.Context, srcDir, dstDir string) error {
	ci := fs.GetConfig(ctx)
	if j.move && operations.SameConfig(j.fsrc, j.fdst) && filter.GetConfig(ctx).InActive() {
		if doDirMove := j.fdst.Features().DirMove; doDirMove != nil {
			err := doDirMove(ctx, j.fsrc, srcDir, dstDir)
			switch {
			case err == nil:
				fs.Infof(j.fdst, "Server side directory move succeeded")
				return nil
			case !errors.Is(err, fs.ErrorCantDirMove) && !errors.Is(err, fs.ErrorDirExists):
				return err
			}
			fs.Infof(j.fdst, "Server side directory move failed - moving files: %v", err)
		}
	}

	var files []fs.Object
	var dirs []string
	err := walk.ListR(ctx, j.fsrc, srcDir, false, ci.MaxDepth, walk.ListAll, func(entries fs.DirEntries) error {
		for _, entry := range entries {
			switch x := entry.(type) {
			case fs.Object:
				if !strings.HasSuffix(x.Remote(), PartialSuffix) {
					files = append(files, x)
				}
			case fs.Directory:
				dirs = append(dirs, x.Remote())
			}
		}
		return nil
	})
	if err != nil {
		return err
	}
	// The remote of an entry under srcDir, on the destination.
	target := func(remote string) string {
		if srcDir != "" {
			remote = strings.TrimPrefix(remote, srcDir+"/")
		}
		return path.Join(dstDir, remote)
	}
	if err := j.fdst.Mkdir(ctx, dstDir); err != nil {
		return err
	}
	slices.Sort(dirs)
	if j.createEmptySrcDirs {
		for _, d := range dirs {
			if err := j.fdst.Mkdir(ctx, target(d)); err != nil {
				return err
			}
		}
	}

	errs := j.files(ctx, files, target)
	if err := ctx.Err(); err != nil {
		return err
	}
	if len(errs) > 0 {
		if len(errs) == 1 {
			return errs[0]
		}
		return fmt.Errorf("%d files failed, the first: %w", len(errs), errs[0])
	}
	if j.move {
		// Deepest first; any that is not empty stays.
		for _, d := range slices.Backward(dirs) {
			if err := operations.TryRmdir(ctx, j.fsrc, d); err != nil {
				fs.Debugf(fs.LogDirName(j.fsrc, d), "Failed to Rmdir: %v", err)
			}
		}
	}
	return nil
}

// files copies or moves files to their target remotes, as many at once as rclone's
// transfers setting says, and returns the errors.
func (j *job) files(ctx context.Context, files []fs.Object, target func(string) string) []error {
	stats := accounting.Stats(ctx)
	var mu sync.Mutex
	var errs []error
	queued, queuedBytes := len(files), int64(0)
	for _, f := range files {
		queuedBytes += max(f.Size(), 0)
	}
	stats.SetTransferQueue(queued, queuedBytes)
	defer stats.SetTransferQueue(0, 0)

	todo := make(chan fs.Object)
	var wg sync.WaitGroup
	for range max(fs.GetConfig(ctx).Transfers, 1) {
		wg.Go(func() {
			for f := range todo {
				mu.Lock()
				queued, queuedBytes = queued-1, queuedBytes-max(f.Size(), 0)
				stats.SetTransferQueue(queued, queuedBytes)
				mu.Unlock()
				if err := j.file(ctx, target(f.Remote()), f); err != nil {
					fs.Errorf(f, "rse: %v", err)
					mu.Lock()
					errs = append(errs, err)
					mu.Unlock()
				}
			}
		})
	}
feed:
	for _, f := range files {
		select {
		case todo <- f:
		case <-ctx.Done():
			break feed
		}
	}
	close(todo)
	wg.Wait()
	return errs
}
