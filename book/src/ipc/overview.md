# IPC

> _This page is a stub. The IPC documentation will be written from the kernel
> source (`kernel/src/ipc/`, `minix-ipc/`) as the corresponding code
> stabilizes._

minix.rs processes communicate exclusively through message passing. Five of
MINIX 3's six primitives are live — `SEND`, `RECEIVE`, `SENDREC`, `NOTIFY`,
`SENDNB` — with `SENDA` still stubbed (`ENOSYS`). Message layout, endpoints,
and call numbers are preserved from MINIX 3.
