# IPC

> _This page is a stub. The IPC documentation will be written from the kernel
> source (`kernel/src/ipc/`, `minix-ipc/`) as the corresponding code
> stabilizes._

minix.rs processes communicate exclusively through six message-passing
primitives: `SEND`, `RECEIVE`, `SENDREC`, `NOTIFY`, `SENDNB`, and `SENDA`.
Message layout, endpoints, and call numbers are preserved from MINIX 3.
