/* MiniOS 链接脚本：OpenSBI (FW_DYNAMIC) 在 QEMU virt 平台上把内核加载到 0x80200000 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;

    .text : {
        *(.text.entry)
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    .data : ALIGN(4K) {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    .bss : ALIGN(4K) {
        _bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        . = ALIGN(16);
        . += 0x8000;              /* 在内核 bss 尾部保留 32 KiB 栈空间 */
        _stack_top = .;
        _bss_end = .;
    }

    /DISCARD/ : {
        *(.eh_frame .eh_frame.*)
        *(.comment .comment.*)
    }
}
