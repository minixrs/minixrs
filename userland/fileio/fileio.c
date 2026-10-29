/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
/*
 * minix.rs `fileio` -- C file I/O through the musl fork (pre-Phase-6 chunk 5).
 *
 * `hello` proved a C program can WRITE to the console. This one proves it can
 * open, read, seek in and write a FILE: open/read/readv/lseek/write/close out
 * of /etc on the MinixFS root, through the fork's syscall shim, VFS and MFS.
 * Exec'd by init out of the filesystem (/bin/fileio), exactly as hello is.
 *
 * This is the RAW section (design M10): every check reports through write(2)
 * on fd 1 directly, never through stdio, so its markers are on the console
 * before stdio or malloc has run at all. A crash in a later section cannot
 * swallow them.
 *
 * Each check prints `minix.rs fileio: <name> ok` only when the thing it names
 * actually worked, or `minix.rs fileio: <name> FAIL <detail>` -- the
 * self-checking 5.3/5.4 style. The FAIL forms are on tests/qemu-boot.forbidden.
 *
 * errno is cleared before every errno-checked call. Nothing in this section
 * mallocs, but a later one will, and mallocng's first allocation leaves errno
 * at ENOSYS (its ignored MAP_FIXED guard mmap) -- a probe that read errno
 * without clearing it first would be judging someone else's failure.
 *
 * Nothing here is minix.rs-specific, as in hello: no <minixrs/...> header,
 * just POSIX. The few facts about the image it depends on are literals, each
 * commented with the kernel-shared constant it mirrors, because the C headers
 * do not carry them.
 */

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

/* ROOTFS_MOTD_PATH and ROOTFS_MOTD (kernel-shared/src/rootfs.rs), byte for
 * byte. The MFS self-check's `n=31` marker pins the Rust side's length. */
static const char motd_path[] = "/etc/motd";
static const char motd[] = "minix.rs rootfs: motd from MFS\n";
#define MOTD_LEN (sizeof motd - 1)

/* ROOTFS_PATTERN_PATH; byte i of it is rootfs_pattern_byte(i) = i % 251. */
static const char pattern_path[] = "/etc/pattern";
#define PATTERN_BYTE(i) ((unsigned char)((i) % 251))

/* ROOTFS_SEEKHOLE_PATH, ROOTFS_SEEKHOLE_OFFSET and ROOTFS_SEEKHOLE_TAIL_LEN:
 * the file this program creates, and where its one block of data goes. One
 * whole block in, so the leading block is a hole MFS never allocates. */
static const char seekhole_path[] = "/etc/seekhole";
#define SEEKHOLE_OFFSET 4096
static const char seekhole_tail[] = "seekhole";
#define SEEKHOLE_TAIL_LEN (sizeof seekhole_tail - 1)

/* Past the end of any file the image can hold -- the whole image is 1 MiB
 * (ROOTFS_IMAGE_BYTES). Seeking there is legal; reading there is end of file. */
#define PAST_EOF ((off_t)1 << 20)

/* ------------------------------------------------------------------------- */
/* Output: one write(2) per line, so a line cannot be split by another writer. */

static char line[192];
static size_t line_len;

static void put_str(const char *s)
{
	size_t n = strlen(s);

	if (n > sizeof line - line_len)
		n = sizeof line - line_len;
	memcpy(line + line_len, s, n);
	line_len += n;
}

static void put_bytes(const char *s, size_t n)
{
	if (n > sizeof line - line_len)
		n = sizeof line - line_len;
	memcpy(line + line_len, s, n);
	line_len += n;
}

static void put_num(long v)
{
	char digits[24];
	size_t i = 0;
	unsigned long u;

	if (v < 0) {
		put_str("-");
		u = 0UL - (unsigned long)v;
	} else {
		u = (unsigned long)v;
	}
	do {
		digits[i++] = (char)('0' + u % 10);
		u /= 10;
	} while (u);
	while (i)
		put_bytes(&digits[--i], 1);
}

static void begin(const char *name)
{
	line_len = 0;
	put_str("minix.rs fileio: ");
	put_str(name);
	put_str(" ");
}

static void emit(void)
{
	(void)write(1, line, line_len);
}

static void ok(const char *name)
{
	begin(name);
	put_str("ok\n");
	emit();
}

/* `<name> FAIL <what> rc=<rc> errno=<errno>` -- every failure names the step
 * that went wrong and what the call returned, so a FAIL line localizes on its
 * own. */
static void fail(const char *name, const char *what, long rc, int err)
{
	begin(name);
	put_str("FAIL ");
	put_str(what);
	put_str(" rc=");
	put_num(rc);
	put_str(" errno=");
	put_num(err);
	put_str("\n");
	emit();
}

/* ------------------------------------------------------------------------- */

/*
 * read: open + read of /etc/motd, written to fd 1 behind a prefix.
 *
 * The bytes printed are the bytes READ, not the literal -- the literal only
 * decides whether to print them. So the marker's tail is MFS's content
 * arriving at the console through this process.
 *
 * Also the zero-length read (Review Focus 2): read(fd, NULL, 0) must answer 0.
 * The shim must not pre-fault a zero-length buffer -- a NULL pre-fault would
 * kill this process -- and VFS answers an empty read before looking at the
 * pointer.
 *
 * Mutation: a shim that stops stripping O_LARGEFILE makes the open EINVAL.
 */
static void check_read(void)
{
	char buf[64];
	ssize_t n;
	int fd;

	errno = 0;
	fd = open(motd_path, O_RDONLY);
	if (fd < 0) {
		fail("read", "open", fd, errno);
		return;
	}

	errno = 0;
	n = read(fd, buf, sizeof buf);
	if (n != (ssize_t)MOTD_LEN || memcmp(buf, motd, MOTD_LEN) != 0) {
		fail("read", "content", n, errno);
		(void)close(fd);
		return;
	}

	errno = 0;
	if ((n = read(fd, NULL, 0)) != 0) {
		fail("read", "zero-len", n, errno);
		(void)close(fd);
		return;
	}

	(void)close(fd);
	begin("read");
	put_bytes(buf, MOTD_LEN);
	emit();
}

/*
 * lseek: SEEK_CUR reports where the read left the descriptor; SEEK_SET 0 and a
 * re-read return the identical bytes; a seek past end of file is legal and the
 * read there returns 0.
 *
 * Mutation: a shim that ignores whence (sends everything as SEEK_SET) answers
 * 0 for the SEEK_CUR query.
 */
static void check_lseek(void)
{
	char first[64], again[64];
	ssize_t n;
	off_t pos;
	int fd;

	errno = 0;
	fd = open(motd_path, O_RDONLY);
	if (fd < 0) {
		fail("lseek", "open", fd, errno);
		return;
	}

	errno = 0;
	n = read(fd, first, sizeof first);
	if (n != (ssize_t)MOTD_LEN) {
		fail("lseek", "read", n, errno);
		goto out;
	}

	errno = 0;
	if ((pos = lseek(fd, 0, SEEK_CUR)) != (off_t)MOTD_LEN) {
		fail("lseek", "cur", pos, errno);
		goto out;
	}

	errno = 0;
	if ((pos = lseek(fd, 0, SEEK_SET)) != 0) {
		fail("lseek", "set", pos, errno);
		goto out;
	}

	errno = 0;
	n = read(fd, again, sizeof again);
	if (n != (ssize_t)MOTD_LEN || memcmp(first, again, MOTD_LEN) != 0) {
		fail("lseek", "reread", n, errno);
		goto out;
	}

	errno = 0;
	if ((pos = lseek(fd, PAST_EOF, SEEK_SET)) != PAST_EOF) {
		fail("lseek", "past-eof", pos, errno);
		goto out;
	}

	errno = 0;
	if ((n = read(fd, again, sizeof again)) != 0) {
		fail("lseek", "eof-read", n, errno);
		goto out;
	}

	ok("lseek");
out:
	(void)close(fd);
}

/*
 * lseek deny: five refusals, each checked as -1 AND the right errno, end to end
 * through syscall_ret -- VFS_LSEEK's contract (design M2).
 *
 *   SEEK_END             EINVAL  (no request reports a file's size yet)
 *   whence 3             EINVAL  (not a whence at all)
 *   SEEK_SET -1          EINVAL  (a negative result)
 *   fd 1                 ESPIPE  (the console is not a file)
 *   a just-closed fd     EBADF
 */
#define LSEEK_DENY_PROBES 5

static int deny(const char *what, int fd, off_t off, int whence, int want)
{
	off_t r;

	errno = 0;
	r = lseek(fd, off, whence);
	if (r == -1 && errno == want)
		return 1;
	fail("lseek deny", what, r, errno);
	return 0;
}

static void check_lseek_deny(void)
{
	int denied = 0;
	int fd;

	errno = 0;
	fd = open(motd_path, O_RDONLY);
	if (fd < 0) {
		fail("lseek deny", "open", fd, errno);
		return;
	}

	if (!deny("seek-end", fd, 0, SEEK_END, EINVAL))
		goto out;
	denied++;
	if (!deny("bad-whence", fd, 0, 3, EINVAL))
		goto out;
	denied++;
	if (!deny("negative", fd, -1, SEEK_SET, EINVAL))
		goto out;
	denied++;
	if (!deny("console", 1, 0, SEEK_CUR, ESPIPE))
		goto out;
	denied++;

	(void)close(fd);
	if (!deny("closed", fd, 0, SEEK_SET, EBADF))
		return;
	denied++;

	if (denied == LSEEK_DENY_PROBES) {
		begin("lseek deny");
		put_str("ok n=");
		put_num(denied);
		put_str("\n");
		emit();
	}
	return;
out:
	(void)close(fd);
}

/*
 * readv across a short read in the middle of a file (Review Focus 3).
 *
 * iov[0] asks for more than one FS transfer can carry, so VFS answers short --
 * one block, FS_MAX_IO -- with /etc/pattern's bytes still to come. readv must
 * stop there: the count is the first transfer's, iov[0] holds exactly that many
 * pattern bytes, and iov[1] -- pre-filled with 0xAA -- is untouched. A readv
 * that read on would put the next bytes in the wrong iovec.
 *
 * The count is printed rather than compared against a literal here: C cannot
 * see FS_MAX_IO, so the expected-marker file is what pins it, recomputed from
 * that constant. What C does check is that the read really WAS short (fewer
 * bytes than iov[0] asked for), since otherwise iov[1] being filled would be
 * legal and the probe would test nothing.
 *
 * Mutation: a shim readv that continues past a short read fills iov[1].
 */
#define READV_LEN0 5000
#define READV_LEN1 100
#define POISON 0xAA

static unsigned char rv0[READV_LEN0];
static unsigned char rv1[READV_LEN1];

static void check_readv(void)
{
	struct iovec iov[2];
	ssize_t n;
	size_t i;
	int fd;

	memset(rv0, POISON, sizeof rv0);
	memset(rv1, POISON, sizeof rv1);
	iov[0].iov_base = rv0;
	iov[0].iov_len = sizeof rv0;
	iov[1].iov_base = rv1;
	iov[1].iov_len = sizeof rv1;

	errno = 0;
	fd = open(pattern_path, O_RDONLY);
	if (fd < 0) {
		fail("readv", "open", fd, errno);
		return;
	}

	errno = 0;
	n = readv(fd, iov, 2);
	(void)close(fd);

	/* iov[1] first: a readv that read on past the short transfer still
	 * returns fewer bytes than iov[0] asked for, so this is the check that
	 * names it. */
	for (i = 0; i < sizeof rv1; i++)
		if (rv1[i] != POISON) {
			fail("readv", "iov1-touched", n, errno);
			return;
		}
	if (n <= 0 || (size_t)n >= sizeof rv0) {
		fail("readv", "not-short", n, errno);
		return;
	}
	for (i = 0; i < (size_t)n; i++)
		if (rv0[i] != PATTERN_BYTE(i)) {
			fail("readv", "iov0", (long)i, 0);
			return;
		}

	begin("readv");
	put_str("ok n=");
	put_num(n);
	put_str("\n");
	emit();
}

/*
 * fresh-buf: read() into a page this program has never touched (design M15).
 *
 * VM backs mmap pages lazily, on first touch, but VFS reaches a caller's buffer
 * through the kernel copy engine, which answers EFAULT for an unmapped page
 * rather than asking VM. The shim pre-faults every buffer it hands VFS; this is
 * the probe that depends on it, so M15 is not a workaround nothing checks.
 * Nothing between the mmap and the read may touch `p`.
 *
 * Then the other half of Review Focus 1: a read into .rodata. The pre-fault
 * only LOADS, which a read-only page allows, so the copy's own write-permission
 * check is what refuses it -- -1/EFAULT, not a dead process.
 *
 * Mutation: removing prefault() from the shim's read makes the first read
 * answer EFAULT.
 */
static const unsigned char rodata_buf[64] = { 0x5A };

static void check_fresh_buf(void)
{
	unsigned char *p;
	ssize_t n;
	int fd;

	errno = 0;
	p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (p == MAP_FAILED) {
		fail("fresh-buf", "mmap", -1, errno);
		return;
	}

	errno = 0;
	fd = open(motd_path, O_RDONLY);
	if (fd < 0) {
		fail("fresh-buf", "open", fd, errno);
		goto unmap;
	}

	errno = 0;
	n = read(fd, p, 64);
	if (n != (ssize_t)MOTD_LEN || memcmp(p, motd, MOTD_LEN) != 0) {
		fail("fresh-buf", "read", n, errno);
		goto out;
	}

	errno = 0;
	if (lseek(fd, 0, SEEK_SET) != 0) {
		fail("fresh-buf", "rewind", -1, errno);
		goto out;
	}

	errno = 0;
	n = read(fd, (void *)rodata_buf, sizeof rodata_buf);
	if (n != -1 || errno != EFAULT) {
		fail("fresh-buf", "rodata", n, errno);
		goto out;
	}

	ok("fresh-buf");
out:
	(void)close(fd);
unmap:
	(void)munmap(p, 4096);
}

/*
 * seek-hole: the first write past end of file through lseek, from anything.
 *
 * Create (or truncate -- this re-runs every fileio exec) a scratch file, seek
 * one whole block in, write a tail, seek back to 0 and read the lot: the
 * leading block must read as zeros (a hole MFS never allocated) and the tail as
 * written, and the file must end exactly there. Also proves O_CREAT and O_TRUNC
 * pass the shim's flag translation untouched: O_CREAT because the first run's
 * open would otherwise be ENOENT, O_TRUNC because the file must read empty
 * straight after the open on every later run.
 *
 * One FS transfer is at most one block, so the read loops; the extra read at
 * the end must be 0, which is the size having reached the inode.
 */
#define SEEKHOLE_TOTAL (SEEKHOLE_OFFSET + SEEKHOLE_TAIL_LEN)

static unsigned char sh[SEEKHOLE_TOTAL + 1];

static void check_seek_hole(void)
{
	size_t got = 0;
	ssize_t n;
	off_t pos;
	size_t i;
	int fd;

	memset(sh, POISON, sizeof sh);

	errno = 0;
	fd = open(seekhole_path, O_RDWR | O_CREAT | O_TRUNC, 0644);
	if (fd < 0) {
		fail("seek-hole", "open", fd, errno);
		return;
	}

	/* Empty on open, every run: without this a lost O_TRUNC would pass,
	 * because the previous run left the very bytes this one writes. */
	errno = 0;
	if ((n = read(fd, sh, 1)) != 0) {
		fail("seek-hole", "not-truncated", n, errno);
		goto out;
	}

	errno = 0;
	if ((pos = lseek(fd, SEEKHOLE_OFFSET, SEEK_SET)) != SEEKHOLE_OFFSET) {
		fail("seek-hole", "seek", pos, errno);
		goto out;
	}

	errno = 0;
	n = write(fd, seekhole_tail, SEEKHOLE_TAIL_LEN);
	if (n != (ssize_t)SEEKHOLE_TAIL_LEN) {
		fail("seek-hole", "write", n, errno);
		goto out;
	}

	errno = 0;
	if ((pos = lseek(fd, 0, SEEK_SET)) != 0) {
		fail("seek-hole", "rewind", pos, errno);
		goto out;
	}

	while (got < SEEKHOLE_TOTAL) {
		errno = 0;
		n = read(fd, sh + got, SEEKHOLE_TOTAL - got);
		if (n <= 0) {
			fail("seek-hole", "read", n, errno);
			goto out;
		}
		got += (size_t)n;
	}

	errno = 0;
	if ((n = read(fd, sh + got, 1)) != 0) {
		fail("seek-hole", "eof", n, errno);
		goto out;
	}

	for (i = 0; i < SEEKHOLE_OFFSET; i++)
		if (sh[i] != 0) {
			fail("seek-hole", "hole", (long)i, 0);
			goto out;
		}
	if (memcmp(sh + SEEKHOLE_OFFSET, seekhole_tail, SEEKHOLE_TAIL_LEN) != 0) {
		fail("seek-hole", "tail", SEEKHOLE_OFFSET, 0);
		goto out;
	}

	ok("seek-hole");
out:
	(void)close(fd);
}

int main(void)
{
	/* Each check opens and closes its own descriptors, so one failing leaves
	 * the next its own clean start -- and the process exits with nothing
	 * open (VFS's exit reset reports only when it closed something). */
	check_read();
	check_lseek();
	check_lseek_deny();
	check_readv();
	check_fresh_buf();
	check_seek_hole();
	return 0;
}
