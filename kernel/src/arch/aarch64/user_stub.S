/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
/*
 * EL0 stubs for slice 2.5 ping-pong demo.
 *
 * Two PC-relative-free blobs, each copied into its own physical page by
 * `userland_bootstrap` (mapped EL0 RX at USER_CODE_VA_{A,B}). The stubs
 * exercise all three sync IPC primitives:
 *
 *   Stub A — sender: three one-shot slice-5.1 bad-pointer probes (see the
 *   comments at _user_stub_a_start), then SENDREC to endpoint B in a loop,
 *   incrementing a counter held in x19 (AAPCS callee-saved; the SVC
 *   entry/exit code in trap.S saves and restores all of x0..x30, so x19
 *   survives the trap).
 *
 *   Stub B — echo server: RECEIVE from ANY, then SEND back to whoever
 *   m_source identifies. Mirrors m_type unchanged.
 *
 *   Stub C — kernel-call client (slice 2.6): SENDREC to the SYSTEM
 *   endpoint with m_type = SYS_GETINFO and payload[0..4] = GET_WHOAMI in
 *   a loop. The kernel dispatcher writes endpoint, priv-flags, init-flags,
 *   and name back into the payload; the reply's m_type is OK (0).
 *
 *   Stub D — page-fault demo (slice 3.4b): touches an unmapped heap page at
 *   0x0100_0000 in a loop. The first store faults; the kernel routes a
 *   VM_PAGEFAULT to the user-space VM server, which maps the page via
 *   SYS_VMCTL and clears D's fault, after which the store retries and
 *   succeeds. Later iterations hit the now-mapped page with no further
 *   faults. D issues no SVC of its own.
 *
 * Each stub carves its 104-byte Message buffer out of [SP_EL0 - 104,
 * SP_EL0). SP stays 16-byte aligned (104 rounds to nothing modulo 16;
 * we never sub more than 104 so the stack top remains usable). The stack
 * pages are 4 KiB so we have headroom.
 *
 * Endpoint constants below are the boot endpoints of slots 11, 12, and
 * the SYSTEM kernel task (-2). make_endpoint(0, p) = p & 0x7FFF, so
 * SYSTEM at gen 0 = (-2 & 0x7FFF) = 0x7FFE. ANY's encoding is
 * `(1 << 14) - 1 = 0x3FFF` (ENDPOINT_SLOT_TOP at generation 0).
 */

    .equ SEND,         1
    .equ RECEIVE,      2
    .equ SENDREC,      3
    .equ ANY,          0x3FFF
    .equ ENDPOINT_A,   11
    .equ ENDPOINT_B,   12
    .equ ENDPOINT_PM,  0                   // boot_endpoint(PM_PROC_NR = 0)
    .equ ENDPOINT_VM,  7                   // boot_endpoint(VM_PROC_NR = 7)
    .equ ENDPOINT_SYS, 0x7FFE              // boot_endpoint(SYSTEM)
    .equ SYS_GETINFO,  0x600               // KERNEL_CALL + 0
    .equ GET_WHOAMI,   12
    .equ PM_GETPID,    0x700               // PM_RQ_BASE + 0
    .equ PM_FORK,      0x701               // PM_RQ_BASE + 1
    .equ PM_EXIT,      0x702               // PM_RQ_BASE + 2
    .equ PM_WAIT,      0x703               // PM_RQ_BASE + 3
    .equ PM_EXEC,      0x704               // PM_RQ_BASE + 4
    .equ VM_BRK,       0xC01               // VM_RQ_BASE + 1
    .equ VM_MMAP,      0xC02               // VM_RQ_BASE + 2
    .equ VM_MUNMAP,    0xC03               // VM_RQ_BASE + 3
    .equ MSG_SIZE,     104                 // sizeof(Message); fits in `mov` imm12

/*
 * Slice-5.1 fault-safety probe addresses (stub A only).
 *
 * A's address space holds exactly two mapped pages: code RX at
 * USER_CODE_VA_A (0x0040_0000) and stack RW at USER_STACK_VA_A
 * (0x0080_0000), with SP_EL0 = 0x0080_1000. Everything else below
 * USER_VA_TOP is in range but unmapped -- which is precisely the case that
 * panicked the kernel before the PT-walk copy engine landed.
 */
    .equ BAD_VA_HI,    0x0090              // 0x0090_0000: in range, unmapped
    .equ STACK_VA_HI,  0x0080              // 0x0080_0000: A's stack page
    .equ STRADDLE_LO,  0x0FF8              // 0x0080_0FF8: 8 bytes in, 96 past the end
    .equ CODE_VA_HI,   0x0040              // 0x0040_0000: A's code page, mapped RO

/* ---- Stub A: sender ----------------------------------------------------- */

    .section .rodata.user_stub_a, "a", %progbits
    .balign 4
    .global _user_stub_a_start
    .type _user_stub_a_start, %object
_user_stub_a_start:
    /* ---- slice 5.1: one-shot fault-safety probes ------------------------
     * Four deliberate bad-pointer IPCs, run once before the ping-pong loop.
     * Each must come back EFAULT (-14) in x0 with the kernel still standing;
     * before slice 5.1 the first one took an EL1 data abort and panicked.
     * The kernel traces each as an `[efault ...]` line -- the checked-in
     * boot markers for this slice. Together they cover every arm of the copy
     * engine: unmapped source, multi-page range, deferred write, and the
     * write-permission check.
     */

    /* (1) immediate read path: SEND whose message pointer is unmapped.
     *     `mini_send` copies the message before it inspects the destination,
     *     so B is never involved and A is never queued. */
    movz    x9, #BAD_VA_HI, lsl #16         // 0x0090_0000
    mov     x0, #ENDPOINT_B
    mov     x1, #SEND
    mov     x2, x9
    svc     #0                              // -> x0 == EFAULT

    /* (2) page-straddling read: 8 bytes land in A's mapped stack page, the
     *     other 96 in the unmapped page above it. A Message is only
     *     8-aligned, so this really can happen; it proves the copy engine
     *     walks *every* page of the range, not just the first. */
    movz    x10, #STACK_VA_HI, lsl #16
    movk    x10, #STRADDLE_LO               // 0x0080_0FF8
    mov     x0, #ENDPOINT_B
    mov     x1, #SEND
    mov     x2, x10
    svc     #0                              // -> x0 == EFAULT

    /* (3) deferred write path: a good SEND, then a RECEIVE naming a bad
     *     deliver buffer. `mini_receive` only records the buffer VA; B's
     *     echo is deposited via MF_DELIVERMSG and the *flush* discovers the
     *     bad page, patching EFAULT into A's parked x0. Works under either
     *     scheduling order -- if B is already RECEIVE-blocked the echo
     *     arrives while A is blocked; otherwise A's RECEIVE finds B queued
     *     on its caller_q. Either way the flush is the code under test. */
    sub     x11, sp, #MSG_SIZE
    str     wzr, [x11, #0]                  // m_source -- kernel overwrites
    str     wzr, [x11, #4]                  // m_type = 0
    mov     x0, #ENDPOINT_B
    mov     x1, #SEND
    mov     x2, x11
    svc     #0                              // good buffer: delivers or queues
    mov     x0, #ENDPOINT_B
    mov     x1, #RECEIVE
    mov     x2, x9                          // bad deliver buffer
    svc     #0                              // -> x0 == EFAULT, via the flush

    /* (4) read-only destination: same shape as (3), but the deliver buffer
     *     names A's own *code* page, which is mapped RO_CODE. The page is
     *     present, so only the copy engine's Prot::writable check rejects
     *     it -- the kernel writes through the HHDM alias, which the MMU's
     *     EL0 permission bits do not police, so without that check this
     *     would silently scribble over executable memory. Nothing else in
     *     the boot exercises it. */
    sub     x11, sp, #MSG_SIZE
    str     wzr, [x11, #0]
    str     wzr, [x11, #4]
    mov     x0, #ENDPOINT_B
    mov     x1, #SEND
    mov     x2, x11
    svc     #0                              // good buffer: delivers or queues
    movz    x12, #CODE_VA_HI, lsl #16       // 0x0040_0000 (RO code page)
    mov     x0, #ENDPOINT_B
    mov     x1, #RECEIVE
    mov     x2, x12
    svc     #0                              // -> x0 == EFAULT, nothing written

    /* A's next SENDREC re-records a good deliver_msg_vir, so the probes
     * leave no lasting state and the loop below is unchanged. */

    mov     x19, #0                         // persistent message counter
1:
    // Build Message at [SP_EL0 - 104, SP_EL0): m_source=0, m_type=counter.
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]                   // m_source — kernel overwrites
    str     w19, [x9, #4]                   // m_type   — counter
    mov     x0, #ENDPOINT_B
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0
    add     x19, x19, #1
    b       1b
    .global _user_stub_a_end
_user_stub_a_end:
    .size _user_stub_a_start, _user_stub_a_end - _user_stub_a_start

/* ---- Stub B: echo server ----------------------------------------------- */

    .section .rodata.user_stub_b, "a", %progbits
    .balign 4
    .global _user_stub_b_start
    .type _user_stub_b_start, %object
_user_stub_b_start:
1:
    // Build Message at [SP_EL0 - 104, SP_EL0). Kernel fills it on RECEIVE.
    sub     x9, sp, #MSG_SIZE
    mov     x0, #ANY
    mov     x1, #RECEIVE
    mov     x2, x9
    svc     #0
    // Reply: read m_source out of the delivered message, SEND back with
    // the same m_type already in [x9 + 4].
    ldr     w10, [x9, #0]                   // m_source (signed Endpoint)
    sxtw    x10, w10
    mov     x0, x10
    mov     x1, #SEND
    mov     x2, x9
    svc     #0
    b       1b
    .global _user_stub_b_end
_user_stub_b_end:
    .size _user_stub_b_start, _user_stub_b_end - _user_stub_b_start

/* ---- Stub C: kernel-call client --------------------------------------- */

    .section .rodata.user_stub_c, "a", %progbits
    .balign 4
    .global _user_stub_c_start
    .type _user_stub_c_start, %object
_user_stub_c_start:
    mov     x19, #0                         // persistent call counter
1:
    // Build the request at [SP_EL0 - 104, SP_EL0):
    //   [+0]   m_source — kernel overwrites
    //   [+4]   m_type    = SYS_GETINFO
    //   [+8]   payload[0..4] = GET_WHOAMI
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]
    mov     w10, #SYS_GETINFO
    str     w10, [x9, #4]
    mov     w10, #GET_WHOAMI
    str     w10, [x9, #8]
    mov     x0, #ENDPOINT_SYS
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0
    add     x19, x19, #1
    b       1b
    .global _user_stub_c_end
_user_stub_c_end:
    .size _user_stub_c_start, _user_stub_c_end - _user_stub_c_start

/* ---- Stub D: brk + mmap/munmap + on-demand paging demo (slices 3.5/3.6) -- */

/*
 * Stub D drives the VM server's region tracking, brk, and mmap/munmap:
 *
 *   1. VM_BRK(0x0100_4000): SENDREC to VM. VM creates D's heap region
 *      [0x0100_0000, 0x0100_4000) and replies. No frames are mapped yet.
 *   2. Touch heap page 0 (0x0100_0000): translation fault → kernel blocks D on
 *      RTS_PAGEFAULT and sends VM_PAGEFAULT → VM sees the addr is in D's heap,
 *      maps a page (SYS_VMCTL(VMCTL_PT_MAP)) and clears the fault. D retries
 *      and the store succeeds.
 *   3. VM_BRK(0x0100_8000): grow the heap region to [0x0100_0000, 0x0100_8000).
 *   4. Touch heap page 1 (0x0100_4000): only in range *after* step 3 — faults
 *      once, VM maps it. Had step 3 not grown the heap, this would be an
 *      out-of-region SIGSEGV (VM leaves the faulter blocked).
 *   5. VM_MMAP(0x2000): SENDREC to VM. VM bump-allocates an anonymous region
 *      from MMAP_BASE (0x0200_0000) and replies with the chosen base, which D
 *      stashes in x22.
 *   6. Touch the mmap page (x22): faults once; VM sees it is in the mmap region
 *      and maps it, exactly like the heap.
 *   7. VM_MUNMAP(x22, 0x2000): SENDREC to VM. VM drops the region and unmaps the
 *      one mapped page (SYS_VMCTL(VMCTL_PT_UNMAP)); the second, never-touched
 *      page is a harmless EINVAL inside the kernel.
 *   8. Write the two *heap* pages 32 times, incrementing a counter in x19. The
 *      mmap page is now unmapped, so this steady phase touches only the heap —
 *      no further faults.
 *   9. Deliberately store to x22 (the munmapped page): an out-of-region fault
 *      (slice 4.5). VM's region check fails, VM raises SYS_KILL(D, SIGSEGV),
 *      the kernel queues the ksig toward PM, and PM terminates D via SYS_EXIT.
 *      D never executes past this store.
 *
 * x19/x20/x21/x22 are AAPCS callee-saved and survive the SVC (trap.S saves and
 * restores x0..x30), so x22 (the mmap base) is still valid for the munmap after
 * an intervening SVC. The 104-byte Message is carved from [SP_EL0-104, SP_EL0).
 */

    .section .rodata.user_stub_d, "a", %progbits
    .balign 4
    .global _user_stub_d_start
    .type _user_stub_d_start, %object
_user_stub_d_start:
    // --- brk #1: grow heap to 0x0100_4000 ---
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]                     // m_source — kernel overwrites
    mov     w10, #VM_BRK
    str     w10, [x9, #4]                     // m_type = VM_BRK
    movz    x11, #0x0100, lsl #16
    movk    x11, #0x4000
    str     x11, [x9, #8]                     // payload[0..8] = new_break
    mov     x0, #ENDPOINT_VM
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0

    // --- touch heap page 0 (now in range): faults once, VM maps it ---
    movz    x20, #0x0100, lsl #16             // x20 = 0x0100_0000 (heap page 0)
    mov     x19, #0                           // persistent counter
    str     x19, [x20]

    // --- brk #2: grow heap to 0x0100_8000 ---
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]
    mov     w10, #VM_BRK
    str     w10, [x9, #4]
    movz    x11, #0x0100, lsl #16
    movk    x11, #0x8000
    str     x11, [x9, #8]                     // new_break = 0x0100_8000
    mov     x0, #ENDPOINT_VM
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0

    // --- touch heap page 1 (in range only after brk #2) ---
    movz    x21, #0x0100, lsl #16
    movk    x21, #0x4000                      // x21 = 0x0100_4000 (heap page 1)
    str     x19, [x21]

    // --- mmap: request 0x2000 bytes; VM chooses and returns the base ---
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]                     // m_source — kernel overwrites
    mov     w10, #VM_MMAP
    str     w10, [x9, #4]                     // m_type = VM_MMAP
    mov     x11, #0x2000
    str     x11, [x9, #8]                     // payload[0..8] = len
    mov     x0, #ENDPOINT_VM
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0
    // Reply landed in the same buffer (SP unchanged); payload[0..8] is the base.
    ldr     x22, [x9, #8]                     // x22 = mmap base (survives SVC)

    // --- touch the mmap page: faults once, VM maps it (region contains it) ---
    str     x19, [x22]

    // --- munmap: drop the mmap region and unmap its backing page ---
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]
    mov     w10, #VM_MUNMAP
    str     w10, [x9, #4]                     // m_type = VM_MUNMAP
    str     x22, [x9, #8]                     // payload[0..8] = base
    mov     x11, #0x2000
    str     x11, [x9, #16]                    // payload[8..16] = len
    mov     x0, #ENDPOINT_VM
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0
    // x22's page is now unmapped; the steady phase touches only the heap pages.

    // --- steady phase: write both heap pages 32 times, no further faults ---
    mov     x23, #32
1:
    str     x19, [x20]
    str     x19, [x21]
    add     x19, x19, #1
    subs    x23, x23, #1
    b.ne    1b

    // --- deliberate SIGSEGV (slice 4.5): touch the munmapped mmap page ---
    // Step 7 dropped x22's region, so this store faults out-of-region: VM
    // raises SYS_KILL(D, SIGSEGV) and PM terminates D via SYS_EXIT. The park
    // loop below is unreachable while the kill chain works; it only keeps D
    // contained if a future change ever resumes a SIGSEGV'd process.
    str     x19, [x22]
2:
    b       2b
    .global _user_stub_d_end
_user_stub_d_end:
    .size _user_stub_d_start, _user_stub_d_end - _user_stub_d_start
