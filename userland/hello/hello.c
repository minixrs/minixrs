/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
/*
 * minix.rs `hello` -- the first C program to run on minix.rs (slice 5.6).
 *
 * This is Phase 5's milestone A: a C program compiled against the musl fork
 * (external/musl), exec'd out of the boot archive by `init`, with `printf`
 * reaching the serial console through VFS -> TTY. Everything under it shipped
 * in earlier slices -- grants (5.2), the console driver and CDEV band (5.3),
 * the POSIX write path (5.4), and the SysV initial stack that lets musl's crt
 * run unpatched (5.5).
 *
 * Nothing here is minix.rs-specific. That is the entire point: this is ordinary
 * C against ordinary <stdio.h>, and it works because the libc underneath it was
 * ported, not because the program was.
 *
 * Each line is self-checking in the 5.3/5.4 style -- a marker that only appears
 * when the thing it names actually worked, rather than a trace that appears
 * whenever the code was reached.
 */

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/*
 * Big enough to overflow stdout's buffer (musl's BUFSIZ is 1024), which is what
 * forces __stdio_write to hand writev TWO non-empty iovecs: the pending buffer
 * contents in iov[0] and the new data in iov[1].
 *
 * Without a write this size the iovec loop in the fork's __minixrs_syscall is
 * never exercised at all -- every other write in this program arrives as a
 * single iovec (unbuffered stderr skips the empty iov[0]; the exit-time flush
 * calls f->write(f, 0, 0), so iov[1] is empty). A loop no marker exercises is a
 * loop that can regress silently, which is the slice-5.1 lesson.
 */
#define IOV_LINE_LEN 1200

int main(void)
{
	ssize_t n;
	static char big[IOV_LINE_LEN];
	size_t wrote;

	/*
	 * The milestone.
	 *
	 * This particular line goes out IMMEDIATELY, and the reason is worth
	 * knowing: musl's stdout starts *line* buffered (`.lbf = '\n'`), and it is
	 * __stdout_write -- called for the first flush -- that sets `f->lbf = -1`
	 * after the fork's ioctl answers -ENOTTY. So this write trips the
	 * line-buffered path on the way through, and every printf AFTER it is
	 * fully buffered and reaches the console only when exit() runs
	 * __stdio_exit. The `errno ok` and `iov ok` lines below are the ones that
	 * prove the flush path.
	 */
	printf("minix.rs hello: Hello from C!\n");

	/* stderr is unbuffered, so this one goes out immediately too -- a
	 * different descriptor and a different buffering mode through the same VFS
	 * path. */
	fprintf(stderr, "minix.rs hello: stderr works\n");

	/*
	 * The two-iovec path -- the only thing in this program that makes the
	 * fork's iovec loop matter, and it has to be set up deliberately.
	 *
	 * __stdio_write hands writev iovs[0] = whatever is PENDING in the stream
	 * buffer and iovs[1] = the new data. Both are non-empty only when a write
	 * too big for the remaining buffer arrives while bytes are still pending.
	 * So: leave a short line pending (stdout is fully buffered by now, so it
	 * stays), then write a block that cannot fit beside it.
	 *
	 * Get this wrong -- write the big block while the buffer is empty -- and
	 * iovs[0] is skipped, the call carries ONE iovec, and a writev that
	 * ignores every iovec after the first passes anyway.
	 *
	 * Two independent halves of the proof, the slice-5.4 shape: the `iov-end`
	 * tail only reaches the console if the SECOND iovec was sent, and
	 * `match=1` says fwrite accounted for every byte. Neither subsumes the
	 * other.
	 */
	printf("minix.rs hello: iov ");
	memset(big, '.', sizeof big);
	memcpy(big + sizeof big - 8, "iov-end\n", 8);
	wrote = fwrite(big, 1, sizeof big, stdout);
	printf("minix.rs hello: iov %s\n",
	       wrote == sizeof big ? "ok match=1" : "FAIL");

	/*
	 * The errno identity check (phase-5 decision D7), at RUNTIME.
	 *
	 * D7 makes minix.rs's POSIX errno magnitudes identical to Linux/musl's,
	 * which is what lets musl's stock bits/errno.h and syscall_ret.c work
	 * unmodified. tools/build-musl.sh proves that at compile time; this
	 * proves the whole chain end to end -- VFS answers EBADF, the driver
	 * relays it, the kernel returns it, syscall_ret.c turns the negative
	 * return into errno, and the value C sees is the one C expects.
	 *
	 * fd 3 is deliberate: it is inside VFS's fd row but not open, so it is
	 * the one descriptor that is well-formed in every respect except being
	 * open. (init's `bad-fd` denial probe uses the same descriptor.)
	 */
	errno = 0;
	n = write(3, "x", 1);
	if (n == -1 && errno == EBADF)
		printf("minix.rs hello: errno ok\n");
	else
		printf("minix.rs hello: errno FAIL n=%d errno=%d\n",
		       (int)n, errno);

	/* Returning from main runs exit(), which flushes stdout and then issues
	 * PM_EXIT. init reaps us and forks the next cycle. */
	return 0;
}
