/*
 * EL0 stub task for slice 2.3.
 *
 * Lives in `.rodata.user_stub` inside the kernel image; `userland_bootstrap`
 * copies these bytes into the user code page (mapped EL0 RX at
 * USER_CODE_VA) before erets. Every instruction here is PC-relative-free, so
 * the same encoding works at any VA — no relocations required.
 *
 * What it does:
 *   - x0 = 0x7FFE  (boot endpoint for SYSTEM, i.e. make_endpoint(0, -2))
 *   - x1 = 3        (SENDREC IPC primitive)
 *   - x2 = 0        (no message pointer; the slice-2.3 do_ipc stub ignores it)
 *   - svc #0        (trap into the kernel SVC handler)
 *   - b _user_stub_start  (loop forever to keep generating do_ipc calls)
 */

    .section .rodata.user_stub, "a", %progbits
    .balign 4
    .global _user_stub_start
    .type _user_stub_start, %object
_user_stub_start:
    mov     x0, #0x7FFE
    mov     x1, #3
    mov     x2, xzr
    svc     #0
    b       _user_stub_start
    .global _user_stub_end
_user_stub_end:
    .size _user_stub_start, _user_stub_end - _user_stub_start
