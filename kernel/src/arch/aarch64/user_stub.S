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
 *   Stub D — SYS_VMCTL client (slice 3.3): self-manages a heap page at
 *   0x0100_0000 by issuing VMCTL_PT_MAP / (store) / VMCTL_PT_UNMAP against
 *   its own endpoint in a loop. Exercises the kernel's PT_MAP/PT_UNMAP
 *   subcalls and cycles the frame allocator. (Slice 3.2's on-demand
 *   page-fault path is bypassed — D maps before it touches.)
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
    .equ ENDPOINT_SYS, 0x7FFE              // boot_endpoint(SYSTEM)
    .equ ENDPOINT_D,   14                  // boot_endpoint(stub D); D targets itself
    .equ SYS_GETINFO,  0x600               // KERNEL_CALL + 0
    .equ SYS_VMCTL,    0x608               // KERNEL_CALL + 8
    .equ GET_WHOAMI,   12
    .equ VMCTL_PT_MAP,   1                 // SYS_VMCTL subcall: alloc+map a page
    .equ VMCTL_PT_UNMAP, 2                 // SYS_VMCTL subcall: unmap+free a page
    .equ VMCTL_PROT_WRITE, 1               // PT_MAP prot bit: EL0 writable
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

/* ---- Stub D: SYS_VMCTL client (slice 3.3) ------------------------------ */

/*
 * Stub D self-manages a single heap page at 0x0100_0000 via SYS_VMCTL,
 * targeting itself (ENDPOINT_D). Each loop iteration:
 *   1. VMCTL_PT_MAP  — kernel allocs a fresh frame, maps it RW, returns its PA.
 *   2. store to the page — must NOT fault (it's mapped now).
 *   3. VMCTL_PT_UNMAP — kernel clears the PTE and frees the frame.
 * The map/unmap cycle also stresses the 3.1a frame allocator's free-list
 * (the LIFO list hands the same frame back each iteration, so no exhaustion).
 * D never relies on the slice-3.2 kernel-as-VM heap window (its proc slot's
 * heap_window is EMPTY); a stray fault would halt loudly.
 */

    .section .rodata.user_stub_d, "a", %progbits
    .balign 4
    .global _user_stub_d_start
    .type _user_stub_d_start, %object
_user_stub_d_start:
    movz    x20, #0x0100, lsl #16              // x20 = 0x0100_0000 (heap VA)
    mov     x19, #0                            // persistent counter
1:
    // --- VMCTL_PT_MAP(target=D, vaddr=x20, prot=WRITE) ---
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]                      // m_source — kernel overwrites
    mov     w10, #SYS_VMCTL
    str     w10, [x9, #4]                      // m_type
    mov     w10, #VMCTL_PT_MAP
    str     w10, [x9, #8]                      // payload[0]  subcall
    mov     w10, #ENDPOINT_D
    str     w10, [x9, #12]                     // payload[4]  target endpoint
    str     x20, [x9, #16]                     // payload[8]  vaddr (u64)
    mov     w10, #VMCTL_PROT_WRITE
    str     w10, [x9, #24]                     // payload[16] prot bits
    mov     x0, #ENDPOINT_SYS
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0
    // --- access the now-mapped page (no fault) ---
    str     x19, [x20]
    add     x19, x19, #1
    // --- VMCTL_PT_UNMAP(target=D, vaddr=x20) ---
    sub     x9, sp, #MSG_SIZE
    str     wzr, [x9, #0]
    mov     w10, #SYS_VMCTL
    str     w10, [x9, #4]
    mov     w10, #VMCTL_PT_UNMAP
    str     w10, [x9, #8]                      // payload[0]  subcall
    mov     w10, #ENDPOINT_D
    str     w10, [x9, #12]                     // payload[4]  target endpoint
    str     x20, [x9, #16]                     // payload[8]  vaddr (u64)
    mov     x0, #ENDPOINT_SYS
    mov     x1, #SENDREC
    mov     x2, x9
    svc     #0
    b       1b
    .global _user_stub_d_end
_user_stub_d_end:
    .size _user_stub_d_start, _user_stub_d_end - _user_stub_d_start
