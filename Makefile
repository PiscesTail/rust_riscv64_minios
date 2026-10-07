
TARGET      := riscv64gc-unknown-none-elf
KERNEL_ELF  := target/$(TARGET)/release/minios
KERNEL_BIN  := minios.bin

QEMU        := qemu-system-riscv64

OPENSBI     ?= C:/msys64/ucrt64/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin
OBJCOPY     := riscv64-unknown-elf-objcopy
OBJDUMP     := riscv64-unknown-elf-objdump
SIZE        := riscv64-unknown-elf-size

RUST_DIR := D:/Rust/bin
export PATH := $(RUST_DIR):$(PATH)

.PHONY: all build run run-debug asm size clean

all: build

build:
	cargo build --release
	$(OBJCOPY) -O binary $(KERNEL_ELF) $(KERNEL_BIN)
	@echo "==> $(KERNEL_BIN) ready"

run: build
	$(QEMU) -machine virt -m 128M -smp 1 -nographic \
		-bios $(OPENSBI) -kernel $(KERNEL_BIN)
run-debug: build
	$(QEMU) -machine virt -m 128M -smp 1 -nographic \
		-bios $(OPENSBI) -kernel $(KERNEL_BIN) -s -S

# 反汇编，检查入口 / trap 代码布局
asm: build
	$(OBJDUMP) -d $(KERNEL_ELF) | less

# 查看各段大小
size: build
	$(SIZE) -A $(KERNEL_ELF)

clean:
	cargo clean
	rm -f $(KERNEL_BIN)
