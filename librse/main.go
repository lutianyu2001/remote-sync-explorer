// Command librse is rclone's librclone, built from remote-sync-explorer's own module so
// that rc calls of its own can be added.
//
// It builds into the same C archive as github.com/rclone/rclone/librclone, with the same
// four exported functions, which wrap the same internal package; package main cannot be
// imported, so this file repeats its cgo shims.
//
//	go build -buildmode=c-archive -o librclone.a .
package main

/*
#include <stdlib.h>

struct RcloneRPCResult {
	char*	Output;
	int	Status;
};
*/
import "C"

import (
	"unsafe"

	"github.com/rclone/rclone/librclone/librclone"

	_ "github.com/rclone/rclone/backend/all" // all backends
	_ "github.com/rclone/rclone/cmd/cmount"  // as librclone imports them
	_ "github.com/rclone/rclone/cmd/mount"
	_ "github.com/rclone/rclone/cmd/mount2"
	_ "github.com/rclone/rclone/fs/operations" // operations/* rc calls
	_ "github.com/rclone/rclone/fs/sync"       // sync/* rc calls
	_ "github.com/rclone/rclone/lib/plugin"
)

// RcloneInitialize initializes rclone as a library.
//
//export RcloneInitialize
func RcloneInitialize() {
	librclone.Initialize()
}

// RcloneFinalize finalizes the library.
//
//export RcloneFinalize
func RcloneFinalize() {
	librclone.Finalize()
}

// RcloneRPCResult is returned from RcloneRPC: Output is a JSON object, Status an HTTP
// status (200 for success).
type RcloneRPCResult struct { //nolint:unused
	Output *C.char
	Status C.int
}

// RcloneRPC calls one rc method with a JSON object as its input. The caller frees
// result.Output with RcloneFreeString.
//
//export RcloneRPC
func RcloneRPC(method *C.char, input *C.char) (result C.struct_RcloneRPCResult) {
	output, status := librclone.RPC(C.GoString(method), C.GoString(input))
	result.Output = C.CString(output)
	result.Status = C.int(status)
	return result
}

// RcloneFreeString frees a string returned by RcloneRPC.
//
//export RcloneFreeString
func RcloneFreeString(str *C.char) {
	C.free(unsafe.Pointer(str))
}

// main is required by -buildmode=c-archive and never runs.
func main() {}
