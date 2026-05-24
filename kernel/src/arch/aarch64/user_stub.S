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
 * Each stub carves its 104-byte Message buffer out of [SP_EL0 - 104,
 * SP_EL0). SP stays 16-byte aligned (104 rounds to nothing modulo 16;
 * we never sub more than 104 so the stack top remains usable). The stack
 * pages are 4 KiB so we have headroom.
 *
 * Endpoint constants below are the boot endpoints of slots 11 and 12 —
 * `make_endpoint(0, 11) = 11`, `make_endpoint(0, 12) = 12`. ANY's encoding
 * is `(1 << 14) - 1 = 0x3FFF` (ENDPOINT_SLOT_TOP at generation 0).
 */

    .equ SEND,        1
    .equ RECEIVE,     2
    .equ SENDREC,     3
    .equ ANY,         0x3FFF
    .equ ENDPOINT_A,  11
    .equ ENDPOINT_B,  12
    .equ MSG_SIZE,    104                  // sizeof(Message); fits in `mov` imm12

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
