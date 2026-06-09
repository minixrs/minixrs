/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
/*
 * EL0 stubs for slice 2.5 ping-pong demo.
 *
 * Two PC-relative-free blobs, each copied into its own physical page by
 * `userland_bootstrap` (mapped EL0 RX at USER_CODE_VA_{A,B}). The stubs
 * exercise all three sync IPC primitives:
 *
 *   Stub A — sender: SENDREC to endpoint B in a loop, incrementing a
 *   counter held in x19 (AAPCS callee-saved; the SVC entry/exit code in
 *   trap.S saves and restores all of x0..x30, so x19 survives the trap).
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
    .equ ENDPOINT_VM,  7                   // boot_endpoint(VM_PROC_NR = 7)
    .equ ENDPOINT_SYS, 0x7FFE              // boot_endpoint(SYSTEM)
    .equ SYS_GETINFO,  0x600               // KERNEL_CALL + 0
    .equ GET_WHOAMI,   12
    .equ VM_BRK,       0xC01               // VM_RQ_BASE + 1
    .equ MSG_SIZE,     104                 // sizeof(Message); fits in `mov` imm12

/* ---- Stub A: sender ----------------------------------------------------- */

    .section .rodata.user_stub_a, "a", %progbits
    .balign 4
    .global _user_stub_a_start
    .type _user_stub_a_start, %object
_user_stub_a_start:
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

/* ---- Stub D: brk + on-demand paging demo (slice 3.5) ------------------- */

/*
 * Stub D drives the VM server's region tracking + brk:
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
 *   5. Loop writing both pages, incrementing a counter in x19. No further
 *      faults — the pages stay mapped, so D round-robins like A/B/C.
 *
 * x19/x20/x21 are AAPCS callee-saved and survive the SVC (trap.S saves and
 * restores x0..x30). The 104-byte Message is carved from [SP_EL0-104, SP_EL0).
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

    // --- steady loop: write both pages, no further faults ---
1:
    str     x19, [x20]
    str     x19, [x21]
    add     x19, x19, #1
    b       1b
    .global _user_stub_d_end
_user_stub_d_end:
    .size _user_stub_d_start, _user_stub_d_end - _user_stub_d_start
