#!/usr/bin/env python3
"""Generate a synthetic ~/.claude dataset for screenshots, demos, and smoke runs.

Writes a fully fake Claude Code home under --home (NEVER the real $HOME) so
ccsight can be pointed at it with `HOME=<demo-home> ccsight`. Every identifier
is a generic placeholder: no real project, path, domain, or usage data may
appear here (same motivation as lint #17, applied to public assets).

Deterministic for a given --seed; dates are relative to the run day so the
heatmap and "today" surfaces always look alive.

Usage:
  python3 scripts/demo_data.py --home /tmp/ccsight-demo-home [--days 90]
  HOME=/tmp/ccsight-demo-home target/release/ccsight
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

# ── Generic vocabulary (placeholders only — keep it that way) ───────────────

# Persona: a low-level generalist who lives across the whole stack — kernel,
# hypervisor, MCU firmware, RTL, a compiler backend, device drivers. Each
# project's archetype maps to a MIXED file set so the Languages panel reads
# like real systems work (C + Assembly + a little C++/Rust/Verilog/CUDA,
# plus Make/CMake/TOML), not one language per repo.
PROJECTS = [
    # (cwd, weight, archetype, branches). A few heavily-weighted core repos
    # dominate the Projects panel; a long tail of low-weight repos gives the
    # project count the breadth a real multi-year checkout accumulates.
    ("/home/dev/src/kernel", 40, "os", ["main", "feature/smp-scheduler", "fix/page-fault"]),
    ("/home/dev/src/hypervisor", 22, "vmm", ["main", "feature/nested-virt", "HEAD"]),
    ("/home/dev/src/firmware", 20, "fw", ["main", "fix/i2c-timing"]),
    ("/home/dev/hw/soc-rtl", 14, "rtl", ["main", "feature/alu-pipeline"]),
    ("/home/dev/src/toolchain", 12, "tc", ["main", "feature/riscv-backend"]),
    ("/home/dev/src/drivers", 12, "drv", ["main", "fix/dma-race", "HEAD"]),
    # Long tail (weight 1): archetypes reuse the six FILES/BASH pools above.
    ("/home/dev/src/microkernel", 1, "os", ["main", "feature/capabilities"]),
    ("/home/dev/src/bootloader", 1, "os", ["main", "fix/a20-gate"]),
    ("/home/dev/src/rtos", 1, "os", ["main", "feature/tickless"]),
    ("/home/dev/src/numa-balancer", 1, "os", ["main", "dev"]),
    ("/home/dev/src/mm-compaction", 1, "os", ["main", "fix/thp-split", "HEAD"]),
    ("/home/dev/lab/syscall-tracer", 1, "os", ["main", "dev"]),
    ("/home/dev/src/vmm-lite", 1, "vmm", ["main", "feature/pvclock"]),
    ("/home/dev/src/iommu", 1, "vmm", ["main", "fix/dma-remap"]),
    ("/home/dev/src/virtio-blk", 1, "vmm", ["main", "dev"]),
    ("/home/dev/src/sev-guest", 1, "vmm", ["main", "feature/attestation"]),
    ("/home/dev/fw/bootrom", 1, "fw", ["main", "fix/crc"]),
    ("/home/dev/fw/uefi-payload", 1, "fw", ["main", "dev"]),
    ("/home/dev/fw/power-mgr", 1, "fw", ["main", "feature/dvfs"]),
    ("/home/dev/fw/sensor-hub", 1, "fw", ["main", "fix/i2c-nack"]),
    ("/home/dev/fw/can-stack", 1, "fw", ["main", "dev"]),
    ("/home/dev/fw/usb-gadget", 1, "fw", ["main", "feature/uac2"]),
    ("/home/dev/hw/gpu-rtl", 1, "rtl", ["main", "feature/raster"]),
    ("/home/dev/hw/dsp-core", 1, "rtl", ["main", "dev"]),
    ("/home/dev/hw/noc-fabric", 1, "rtl", ["main", "fix/deadlock"]),
    ("/home/dev/hw/cache-rtl", 1, "rtl", ["main", "feature/mesi"]),
    ("/home/dev/hw/fpu-unit", 1, "rtl", ["main", "dev"]),
    ("/home/dev/src/linker", 1, "tc", ["main", "feature/lto"]),
    ("/home/dev/src/assembler", 1, "tc", ["main", "dev"]),
    ("/home/dev/src/jit", 1, "tc", ["main", "feature/tiering"]),
    ("/home/dev/src/debugger", 1, "tc", ["main", "fix/dwarf", "HEAD"]),
    ("/home/dev/src/llvm-passes", 1, "tc", ["main", "dev"]),
    ("/home/dev/src/gpu-driver", 1, "drv", ["main", "feature/dmabuf"]),
    ("/home/dev/src/nvme-driver", 1, "drv", ["main", "fix/queue-depth"]),
    ("/home/dev/src/net-driver", 1, "drv", ["main", "feature/gro", "HEAD"]),
    ("/home/dev/src/wifi-driver", 1, "drv", ["main", "dev"]),
    ("/home/dev/src/pci-hotplug", 1, "drv", ["main", "fix/aer"]),
    ("/home/dev/src/block-cache", 1, "drv", ["main", "dev"]),
    ("/home/dev/src/tty-layer", 1, "drv", ["main", "fix/flow-control"]),
    ("/home/dev/src/watchdog", 1, "drv", ["main", "dev"]),
]

FILES_BY_LANG = {
    "os": [
        "kernel/sched/core.c", "kernel/sched/fair.c", "mm/page_alloc.c",
        "arch/x86/entry.S", "arch/riscv/head.S", "include/sched.h",
        "drivers/tty/serial.c", "kernel/Kconfig", "Makefile",
    ],
    "vmm": [
        "src/vcpu.rs", "src/vmcs.rs", "src/ept.rs", "src/devices/virtio_net.rs",
        "arch/vmx_entry.S", "src/main.rs", "Cargo.toml",
    ],
    "fw": [
        "src/main.c", "src/i2c.c", "src/spi.c", "src/dma.c",
        "include/registers.h", "startup.S", "link.ld", "Makefile",
    ],
    "rtl": [
        "rtl/cpu/alu.sv", "rtl/cpu/decode.sv", "rtl/cpu/pipeline.sv",
        "rtl/soc/uart.sv", "sim/gemm.cu", "tb/cpu_tb.sv", "Makefile",
    ],
    "tc": [
        "src/codegen.cpp", "src/parser.cpp", "src/regalloc.cpp",
        "include/ir.hpp", "src/riscv/emit.cpp", "cmake/riscv.cmake",
    ],
    "drv": [
        "drivers/net/nic.c", "drivers/gpu/drm.c", "drivers/block/nvme.c",
        "include/pci.h", "include/dma.h", "Makefile",
    ],
}

BASH_BY_LANG = {
    "os": ["make -j$(nproc)", "make ARCH=riscv defconfig",
           "qemu-system-riscv64 -kernel build/kernel.elf -nographic", "objdump -d build/kernel.elf"],
    "vmm": ["cargo build --release", "cargo test", "./scripts/run-guest.sh"],
    "fw": ["make flash", "arm-none-eabi-size build/fw.elf", "openocd -f board.cfg",
           "arm-none-eabi-gdb build/fw.elf"],
    "rtl": ["verilator --cc rtl/cpu/alu.sv", "iverilog -o build/sim tb/cpu_tb.sv",
            "gtkwave build/dump.vcd", "nvcc -arch=sm_80 sim/gemm.cu"],
    "tc": ["cmake --build build", "ctest --test-dir build", "./build/tc -S test.ir"],
    "drv": ["make modules", "sudo insmod build/nic.ko", "dmesg | tail -20", "sudo rmmod nic"],
}

# Conversation arcs: coherent multi-turn exchanges, one pool per project
# archetype so a session's dialogue, summary and custom title match the repo
# it lives in (a kernel session never discusses RTL timing). Big pools keep
# cross-session repetition low — search results and previews surface it.
# Each arc: (summary, [(user, ai)]).
ARCS_BY_ARCH: dict[str, list] = {}

ARCS_BY_ARCH["os"] = [
    ("SMP scheduler run-queue race", [
        ("getting rare list corruption in the scheduler under load on 32 cores — smells like a run-queue race",
         "Agreed. `enqueue_task` walks the run-queue without holding `rq->lock` on the load-balance path, so a concurrent `dequeue` on another CPU can splice a freed node. Tracing the lock order now."),
        ("is it the double-rq lock ordering again?",
         "Yes — the balancer takes the two run-queue locks in pointer order but the wakeup path takes them in CPU-id order. I unified both on pointer order and added a lockdep assertion."),
        ("run it under stress-ng before you trust it",
         "4 hours of stress-ng with lockdep and KASAN on: no splats, no corruption. Patch is ready with the ordering documented above the struct."),
    ]),
    ("Page fault storm on fork", [
        ("fork of a 4GB process is pegging one core in the fault handler",
         "Copy-on-write is faulting every page eagerly because the PTEs aren't marked write-protected on the parent side, so each access re-copies. Fixing the fork path to wrprotect both sides."),
        ("watch out for the hugepage case",
         "Handled — the PMD-level COW splits only on write, and I kept the THP intact until then. fork latency dropped from 180ms to 12ms on the 4GB test."),
    ]),
    ("RCU stall in the block layer", [
        ("getting rcu_sched stall warnings when the box is under heavy fio load",
         "A retry loop in the completion path spins without ever reaching a quiescent state when the queue stays full. Converting it to requeue via the workqueue so the CPU can report quiescence."),
        ("is the stall the cause of the latency spikes or a symptom?",
         "Cause — the stalled CPU was holding up the grace period every callback waited on. With the requeue in place the p99 completion latency dropped back under a millisecond."),
    ]),
    ("Futex missed wakeup", [
        ("a userspace queue blocks forever maybe once per million handoffs",
         "The waiter reads the futex word, decides to sleep, and the waker's store+wake lands in that window — but the kernel-side requeue path compares an unbarriered copy. Adding the missing pairing barrier."),
        ("can you reproduce it deterministically before fixing?",
         "Yes — with a delay injected between the read and the syscall it fires every few hundred iterations. After the barrier fix, ten billion handoffs with zero hangs."),
    ]),
    ("Slab use-after-free in the tty layer", [
        ("KASAN says use-after-free in a tty buffer on hangup, can't see where",
         "The flush work is queued from the ldisc but the buffer is freed on hangup without cancelling it — the work fires into freed memory. Adding cancel_work_sync before the free."),
        ("any other paths that queue that work?",
         "One more on the resize path — same fix applies. KASAN is quiet through an overnight stress of open/hangup cycles."),
    ]),
    ("NUMA balancer thrashing", [
        ("the numa balancer keeps migrating the same pages back and forth",
         "Two threads on different nodes touch the same shared region, so each fault flips placement. I'll widen the fault-window stats so shared pages get pinned to the node with more accesses."),
        ("what does that do to the microbenchmark that regressed last time?",
         "It recovers — migrations on the shared-matrix benchmark drop 30x and runtime lands within a percent of the pinned-placement baseline."),
    ]),
    ("Tickless idle timer drift", [
        ("wall clock drifts a few ms per hour on the tickless config only",
         "The idle path programs the next timer from a cached now instead of re-reading the clocksource, so each long idle accumulates the stale delta. Re-reading at program time fixes it."),
        ("check the wakeup-latency benchmark doesn't regress",
         "Ran it — the extra clocksource read is tens of nanoseconds, invisible next to the wakeup path. Drift is gone over a 12h soak."),
    ]),
    ("Interrupt storm from a level IRQ", [
        ("one core is 100% in irq context after the new board bringup",
         "A level-triggered line is acked but its source never cleared — the handler returns and the line immediately refires. The device clear register needs a write the driver isn't doing."),
        ("mask it until the clear lands?",
         "Did exactly that: mask, clear at the source, unmask. The storm stops and the device still delivers normally under load."),
    ]),
]

ARCS_BY_ARCH["vmm"] = [
    ("virtio-net link up handshake", [
        ("guest sees the virtio-net device but never gets link up",
         "The VMM isn't setting the VIRTIO_NET_S_LINK_UP bit in the config space, and it's ignoring the guest's DRIVER_OK write. Wiring the status handshake and the config-change interrupt now."),
        ("make sure the feature negotiation matches what the guest offers",
         "Masked our offered features to the intersection and rejected the DRIVER_OK if mandatory ones are missing. Guest brings the link up and pings through the tap now."),
    ]),
    ("Nested EPT fault loop", [
        ("the L2 guest livelocks taking the same EPT violation forever",
         "The shadow EPT entry is built from a stale L1 mapping — we install it, L1 invalidates, and we never see the invalidation because the vmcs shadowing misses that field. Trapping the invalidation explicitly."),
        ("does this need a full shadow table flush?",
         "No — only the affected GPA range; a full flush would crater nested performance. The loop is gone and the nested boot proceeds past the initramfs."),
    ]),
    ("pvclock drift in the guest", [
        ("guest clock runs ahead ~2s/day while the host is fine",
         "The pvclock scale factor is computed once at vcpu init but the host TSC gets recalibrated later — the guest keeps the stale multiplier. Updating the pvclock page on recalibration fixes it."),
        ("what happens across live migration?",
         "The destination recomputes the scale on resume, and the guest sees a single small step, not drift. Added a test that migrates under ntp and checks the offset stays bounded."),
    ]),
    ("MSI-X routing drops interrupts", [
        ("passthrough NIC loses interrupts when the guest remaps MSI-X vectors",
         "The remap window is handled but we keep delivering to the old vector until the next vmexit — writes to the mask bit during that window are lost. Flushing the routing cache on the mask write."),
        ("is there a spec-required ordering here?",
         "Yes — mask must be observed before the vector move. With the flush the ordering holds and the interrupt counter matches on both sides through a remap storm."),
    ]),
    ("vmexit storm on cpuid", [
        ("a guest workload is 40% slower virtualized, perf shows cpuid exits",
         "A polling library calls cpuid for TSC serialization on every iteration — each one is a mandatory exit. Exposing the invariant-TSC leaf so the library takes its fast path instead."),
        ("safe to expose that leaf on migration between hosts?",
         "Only within the same TSC-invariant pool — the launch config already pins that, so the leaf is consistent across the pool. Exit rate dropped 200x."),
    ]),
    ("Balloon deflate stall", [
        ("guest OOMs even though the balloon should have deflated",
         "The deflate request races with the guest's own reclaim — the balloon driver waits for pages the guest is still scanning. Splitting the deflate into smaller chunks lets reclaim interleave."),
        ("cap the chunk size or make it adaptive?",
         "Adaptive — start large, halve on timeout. Deflate under memory pressure now completes in bounded time in the stress guest."),
    ]),
    ("Dirty-page tracking for migration", [
        ("live migration never converges on the write-heavy guest",
         "Dirty logging is per-4K but the guest writes span huge pages, so every touch re-dirties 512 entries. Enabling PML with 2M granularity splitting only on the final pass."),
        ("what's the downtime budget looking like?",
         "Pre-copy converges in 9 rounds and the stop-and-copy window fits in ~80ms on the test guest — inside budget with margin."),
    ]),
    ("virtio-blk queue stall under fsync", [
        ("guest disk hangs for seconds when the host filesystem syncs",
         "The vring is drained synchronously on the host flush, blocking new submissions. Moving the flush to a completion callback keeps the queue accepting requests."),
        ("does the guest see write reordering from that?",
         "No — flush completion is still only signalled after the host fsync returns; only submission is decoupled. The multi-second stalls are gone in the trace."),
    ]),
]

ARCS_BY_ARCH["fw"] = [
    ("I2C timing on the temp sensor", [
        ("the temp sensor NAKs about 1 in 50 reads on the bench board",
         "The bus is running at 400kHz but the sensor needs a longer setup time after the repeated-start. I'll drop to 100kHz for that transaction and add the required tSU;STA delay."),
        ("can we keep 400k for the other devices on the bus?",
         "Yes — the rate is per-transfer now, so only the sensor read slows down. 2000 reads on the bench: zero NAKs."),
    ]),
    ("Linker section overflow on the MCU", [
        ("firmware link fails: .data overflows RAM by 2KB after the last change",
         "A big const lookup table got placed in RAM because it's not marked `const` in a header, so it lands in .data instead of .rodata/flash. Fixing the qualifier and checking the map file."),
        ("also give us some headroom in the linker script",
         "Moved the table to flash (saved the 2KB) and added a 4KB `_stack_guard` region with an ASSERT in the .ld so an overflow fails the link loudly instead of at runtime."),
    ]),
    ("Boot hangs before UART init", [
        ("board is dead silent on boot — no UART output at all",
         "The console is used before the UART's clock gate is enabled, so the first prints hang on a not-ready FIFO. Moving clock+pinmux setup ahead of console init in the boot sequence."),
        ("add an early putc that doesn't need interrupts",
         "Added a polled early-putc wired to the boot console, so we get output from the first C code after head.S. It prints the banner now and continues to the scheduler."),
    ]),
    ("Cache coherency on the mailbox", [
        ("two cores talk over a shared mailbox in SRAM and sometimes miss an update",
         "The producer writes the payload then the flag, but without a barrier the consumer can observe the flag before the payload on a weakly-ordered core. Adding a release/acquire pair around the flag."),
        ("this is the non-coherent DMA region, right?",
         "Right — that region isn't cache-coherent, so I also added a cache clean before the flag write and an invalidate before the payload read. The dropped-message rate went to zero overnight."),
    ]),
    ("SPI DMA underrun on burst reads", [
        ("the flash driver returns corrupted pages when the display DMA is busy",
         "Both peripherals share a DMA arbiter and the SPI RX FIFO underruns while it waits its turn. Raising the SPI channel priority and shrinking the burst so the latency fits the FIFO depth."),
        ("verify with the display actively scanning out",
         "Looped 10k page reads with the panel refreshing: zero CRC mismatches, and scan-out shows no tearing since its bandwidth reservation still holds."),
    ]),
    ("Watchdog bites during flash erase", [
        ("unit resets mid-update, looks like the watchdog fires during sector erase",
         "Sector erase stalls the bus long enough that the kick in the main loop misses its window. Kicking from a timer ISR is banned by policy, so I'll split the erase and kick between sectors."),
        ("keep the update atomic though",
         "The staging slot + boot-side CRC already gives atomicity — a reset mid-erase just reboots into the old image. Update completes with the watchdog armed the whole time."),
    ]),
    ("Brownout during flash write", [
        ("devices in the field corrupt config when the battery is nearly dead",
         "The config write happens at a voltage where flash program time goes out of spec. Gating writes on the brownout comparator and journaling so a torn write replays cleanly."),
        ("how much margin does the comparator threshold leave?",
         "Set 200mV above the flash program minimum — the bench supply sweep shows writes either complete or are refused, never torn."),
    ]),
    ("Deep-sleep wake latency", [
        ("wake from deep sleep takes 40ms, spec wants under 10",
         "Most of it is re-initializing peripherals that kept state anyway. Adding a retained-state fast path that skips reconfig when the retention domain held."),
        ("what invalidates the retained state?",
         "A brownout or a debugger attach — both set a flag that forces the full init path. Wake is at 6ms on the bench with the fast path."),
    ]),
]

ARCS_BY_ARCH["rtl"] = [
    ("ALU pipeline hazard", [
        ("sim shows a wrong result when a mul feeds the next add back-to-back",
         "The forwarding path covers the ALU-to-ALU case but not the multiplier, which has an extra cycle of latency — so the add reads the stale value. Adding a stall + forward from the mul's writeback stage."),
        ("prefer a forward over a stall if you can",
         "Done — forwarded from the mul's final stage, no stall needed. The back-to-back testbench passes and the critical path didn't move."),
    ]),
    ("CDC metastability on FIFO pointers", [
        ("the async FIFO drops a word once every few million cycles in gate sim",
         "The read pointer crosses domains through a single flop — a metastable sample skews the gray decode. Adding the second synchronizer stage and re-checking the full/empty margins."),
        ("does the extra stage break the almost-full threshold?",
         "It adds one cycle of pessimism, so I moved the threshold by one entry. Gate sim is clean over the same seed set that used to fail."),
    ]),
    ("UART FIFO overrun in the testbench", [
        ("rx testbench overruns at back-to-back frames with no idle gap",
         "The status read and the pop are two transactions, and a byte can land between them — the level check is stale. Making the pop return the level atomically in the same read."),
        ("is that a testbench artifact or a real bug?",
         "Real — a fast peer with no inter-frame gap hits it on silicon too. The atomic read closes it in both."),
    ]),
    ("MESI livelock in the cache testbench", [
        ("two caches ping-pong the same line forever in the random test",
         "Both issue upgrades simultaneously, both get invalidated, both retry in lockstep — the arbiter's round-robin preserves the tie. Adding a grant-holds-one-extra-cycle rule breaks the symmetry."),
        ("prove it can't starve the third master",
         "The extra cycle only applies to the winner's immediate retry — the rotation still advances, so any master waits at most N-1 grants. Random test runs 10M cycles clean."),
    ]),
    ("Timing closure on the multiplier", [
        ("the mul stage misses timing by 300ps at the target corner",
         "The partial-product compressor tree has one late-arriving row from the booth mux. Re-balancing the tree so that row enters a stage earlier trims the path."),
        ("don't grow the area doing it",
         "Same compressor count, just re-ordered — area is flat and the corner closes with 40ps of margin."),
    ]),
    ("AXI backpressure deadlock", [
        ("the interconnect deadlocks when two masters cross-request under backpressure",
         "Write data is accepted before the address wins arbitration, so each master holds W beats the other needs drained. Enforcing AW-before-W per master removes the cycle."),
        ("does the protocol checker catch this class now?",
         "Added an assertion that W never leads AW by more than the accepted depth — it fires on the old RTL and is silent on the fix."),
    ]),
    ("Branch predictor aliasing", [
        ("two hot branches destructively alias in the BTB on the perf suite",
         "The index hashes only low PC bits and both branches land in the same set. Folding higher bits into the hash spreads them without growing the table."),
        ("what's the win on the suite?",
         "Misprediction on the two affected benchmarks drops ~35% and nothing else moves outside noise."),
    ]),
    ("Scan chain breaks at synthesis", [
        ("DFT reports a broken scan chain after the clock-gating change",
         "The new gated domain has flops whose scan-enable bypass wasn't hooked to the gate override. Adding the test-mode override so the chain shifts with gates forced open."),
        ("coverage number after the fix?",
         "Chain integrity passes and stuck-at coverage is back to the same figure the block had before the gating change."),
    ]),
]

ARCS_BY_ARCH["tc"] = [
    ("RISC-V backend register allocation", [
        ("the new riscv backend spills way more than it should on tight loops",
         "The allocator isn't coalescing the move pairs from phi elimination, so loop-carried values get their own registers and spill. I'll add a coalescing pass before the linear scan."),
        ("keep it linear-time, we compile big files",
         "It's near-linear — union-find over move-related vertices, no interference re-scan. Spills on the benchmark loops dropped ~40% and compile time is within noise."),
    ]),
    ("LTO drops a used symbol", [
        ("the LTO build links, then crashes calling a function that's... gone?",
         "The symbol is only referenced from inline asm, which the IR-level liveness can't see, so LTO internalizes and strips it. Marking asm-referenced symbols as used before internalization."),
        ("can we lint for this instead of finding it at runtime?",
         "Added a link-time check: any symbol named in asm operands must survive to the final object, or the build fails with the referencing TU named."),
    ]),
    ("DWARF line table off-by-one", [
        ("the debugger steps onto the wrong line inside unrolled loops",
         "The unroller copies instructions but keeps the original is_stmt flags, so every copy claims to start the line. Recomputing statement starts per unrolled body fixes stepping."),
        ("does that bloat the line table?",
         "Slightly — a few percent on the worst TU — but stepping is correct, and the table compresses most of it back."),
    ]),
    ("Peephole miscompiles a rotate", [
        ("crypto test vectors fail at -O2 only, fine at -O0",
         "The peephole fuses shl+shr into a rotate but ignores that the shift amount register is modified between them. Adding a def-use check between the paired instructions."),
        ("scan for other fusions with the same blind spot",
         "Audited all two-instruction fusions: two more had the same gap (add+cmp, lea+shift). Same check applied; the vector suite is green at every opt level."),
    ]),
    ("gc-sections strips an ISR", [
        ("the firmware image builds but the timer interrupt never fires",
         "The ISR is referenced only from the vector table, which is assembled from a section the linker considers unreferenced — gc-sections drops the ISR. Adding KEEP on the vector section."),
        ("shouldn't the toolchain warn about dropping a vector entry?",
         "Added a post-link check that every vector slot resolves to a kept symbol — it fails loudly now instead of shipping a dead vector."),
    ]),
    ("Inliner regression on the hot loop", [
        ("the interpreter loop got 8% slower after the inliner tuning",
         "The new threshold inlines a cold helper into the dispatch loop, blowing the loop out of the uop cache. Marking the helper noinline via the profile data restores the layout."),
        ("make the fix profile-driven, not a hardcoded attribute",
         "Done — the inliner now respects block temperature from the profile: cold callees stay out of hot loops. The 8% comes back and the suite is otherwise flat."),
    ]),
    ("JIT tier-up loop", [
        ("the jit keeps recompiling the same method at tier 2 forever",
         "The tier-2 code deopts on an uncommon type, falls to tier 1, re-warms, and re-tiers with the same speculation. Recording the failed speculation so tier-up compiles without it."),
        ("bound the memory for those records",
         "Per-method failure cache capped at a handful of entries with LRU — the loop stops after one deopt and steady-state sits at tier 2."),
    ]),
    ("Cross sysroot misconfiguration", [
        ("cross builds pick up the host's libc headers half the time",
         "The include order puts the host path ahead of the sysroot when a package injects -I/usr/include. Forcing --sysroot resolution first and warning on absolute host includes."),
        ("fail the build instead of warning?",
         "Made it an error under the cross profile — the offending package is patched, and the toolchain now refuses silently-mixed roots."),
    ]),
]

ARCS_BY_ARCH["drv"] = [
    ("DMA completion race in the NIC driver", [
        ("nic driver occasionally reads a descriptor before the DMA write lands",
         "The completion check reads the OWN bit without a barrier, so the CPU sees the descriptor before the device's DMA write is visible. Adding a `dma_rmb()` after the OWN check."),
        ("does that hurt throughput?",
         "Negligible — it's a read barrier on an already-cache-hot line. Line-rate 10GbE holds, and the sporadic checksum errors are gone under an hour of iperf."),
    ]),
    ("NVMe admin queue timeout", [
        ("controller resets randomly: admin command timed out on identify",
         "The admin queue doorbell is written before the command is fully in the submission entry on weakly-ordered platforms. A wmb between the SQE write and the doorbell fixes the ordering."),
        ("why did this never fire on the x86 rig?",
         "x86's store ordering hides it — the arm box reorders the writes. Verified with a 24h soak on both: zero timeouts."),
    ]),
    ("GPU fence leak on context destroy", [
        ("gpu memory climbs until OOM when a game restarts repeatedly",
         "Destroying a context with in-flight work leaks its fences — the completion callback unrefs a fence the destroy path already dropped from the table. Deferring table removal until idle."),
        ("does that delay context destroy noticeably?",
         "Only until the last fence signals — microseconds in practice. The restart loop holds steady memory over a thousand cycles."),
    ]),
    ("Suspend/resume ordering", [
        ("the sensor driver dies on resume when its bus resumes later",
         "Resume order follows probe order, and the sensor probes before the bus on this board. Declaring the bus as a device link supplier makes the PM core order them correctly."),
        ("any other consumers of that bus with the same hazard?",
         "Two more — both get the link from the same helper now. A hundred suspend cycles with rtcwake: no resume errors."),
    ]),
    ("PHY autoneg flaps", [
        ("link on the second port flaps every ~30s against one specific switch",
         "Autoneg restarts because the driver re-applies advertisement on every polled read, which the PHY treats as a renegotiation request. Only writing the advertisement when it actually changes."),
        ("is the switch also misbehaving here?",
         "It's within spec — restarting on advertisement write is allowed. Our write was the bug; link holds for hours now."),
    ]),
    ("MSI affinity lost after hotplug", [
        ("after removing and re-adding the card, all its interrupts land on CPU0",
         "The affinity mask is restored from a stale copy saved before the remove — the re-add allocates new vectors that never get the spread applied. Recomputing the spread on re-add."),
        ("does irqbalance fight with that?",
         "No — managed interrupts are exempt from irqbalance; the kernel spread is authoritative. Post-hotplug the per-queue interrupts land back on their queues' CPUs."),
    ]),
    ("ioctl bounds check", [
        ("fuzzer found an oops in the ioctl path with a huge length field",
         "The length is used to size a copy before it's validated against the buffer cap — classic. Clamping and returning EINVAL before any allocation or copy."),
        ("add the fuzzer case as a regression test",
         "Added it plus the boundary values around the cap. The fuzzer has been quiet on that entry point since."),
    ]),
    ("Block plug latency regression", [
        ("p99 write latency doubled after the batching change",
         "The new plug holds requests until the batch fills, but low-QD workloads never fill it and eat the flush timer. Flushing the plug on queue-idle instead of timer-only."),
        ("keep the throughput win from batching though",
         "High-QD still batches to the full size — only idle queues flush early. Throughput unchanged, p99 back to baseline."),
    ]),
]

# Short trailing exchanges to vary session length beyond a single arc.
FOLLOWUPS = [
    ("thanks, looks good", "Anything else on this? Otherwise I'll clean up the branch."),
    ("lgtm, squash and update the changelog", "Squashed into one commit with the root-cause in the message body."),
    ("why is the build warning now?", "A missing `static` on a helper leaked a symbol — added it, the -Werror build is clean."),
    ("can you double-check the error path?", "Walked it again: the failure branch releases the lock, returns -EINVAL, and leaks nothing. Added an assertion to pin it."),
    ("does this need a defconfig bump?", "No — it's guarded by an existing Kconfig; the default is unchanged."),
    ("rebase on main first, there were conflicts yesterday", "Rebased cleanly — the conflicting hunk was already fixed upstream. CI is rebuilding."),
    ("add a test for the zero-length case", "Added — a zero-length transfer now returns early instead of arming the DMA, with a unit test pinning it."),
    ("what's the perf impact?", "Benchmarked before/after: within noise on the hot path, ~3% faster on the large-transfer path."),
    ("hold on, does this break the older silicon?", "No — the fast path is gated on a feature bit; the older part falls back to the polled path."),
    ("ok, boot it on the board and watch the console", "Flashed it — boots clean, banner prints, and it's been stable through a hundred reset cycles."),
    ("split that into two commits, the rename is drowning the fix", "Split — the mechanical rename first, the behavior change on top with the test."),
    ("can you write up the root cause for the team?", "Drafted a short postmortem: trigger, mechanism, fix, and the assertion that now guards it."),
    ("is there a config where this still fails?", "Swept the config matrix — one debug-only combination still hits it; guarded that path explicitly."),
    ("what does the diff look like size-wise?", "Small: a dozen lines of logic and a test file. The map/asm output is byte-identical outside the fix."),
    ("did CI go green?", "Green across the matrix — the previously flaky job passed ten consecutive runs."),
    ("hmm, I don't buy that explanation — trace it again", "Fair — traced it end to end and the original story was half right; the real trigger is the retry path. Updated the fix accordingly."),
    ("leave a comment explaining the invariant", "Added a comment stating the ordering invariant and which assertion enforces it."),
    ("bench it against the baseline branch", "Ran both branches on the same input set: no regressions, the fixed path is marginally faster."),
    ("does the doc need updating too?", "Yes — one paragraph described the old behavior; rewrote it to match and linked the design note."),
    ("cherry-pick this to the release branch", "Cherry-picked cleanly; release CI is running and the tag is ready once it's green."),
    ("wait, revert that last change first", "Reverted — back to the known-good state; re-applying the pieces one at a time to isolate it."),
    ("show me the numbers before and after", "Before: intermittent failures every few hundred runs. After: clean across the full overnight loop, latency unchanged."),
]

# Keyed like ARCS_BY_ARCH so a hand-written title never contradicts the
# project it's attached to.
CUSTOM_TITLES_BY_ARCH = {
    "os": ["scheduler lock rework", "SMP run-queue race", "MMU fault handler",
           "rcu stall triage", "futex wakeup fix", "numa balance tuning"],
    "vmm": ["nested-virt EPT fault", "virtio-net link up", "pvclock drift fix",
            "vmexit storm hunt", "migration convergence"],
    "fw": ["boot bringup debug", "firmware flash retry path", "i2c timing fix",
           "linker overflow", "deep-sleep wake path"],
    "rtl": ["RTL pipeline hazard", "cache coherency audit", "cdc metastability fix",
            "mul timing closure", "axi deadlock repro"],
    "tc": ["regalloc coalescing", "toolchain LTO regression", "link-time GC pass",
           "dwarf line fix", "peephole audit"],
    "drv": ["DMA descriptor ring fix", "nvme timeout triage", "gpu fence leak",
            "phy autoneg debug", "resume ordering fix"],
}

# Local-hour weights for session starts: evening-peaked with a plateau to
# midnight, a morning shoulder and a lunch dip — the shape of a developer
# who codes after dinner, not a bell curve over office hours.
HOUR_WEIGHTS = [
    1, 0, 0, 0, 0, 0, 1, 2, 4, 6, 7, 7,
    5, 7, 8, 8, 9, 10, 10, 8, 7, 6, 5, 3,
]

# Frontier-model adoption over time: at any point in history the bulk of
# usage goes to that era's daily-driver, with a light-model long tail —
# matching how real usage looks. (days_ago_start, weighted models).
# Keys must exist in aggregator/pricing.rs so costs render (no `$?`).
MODEL_ERAS = [
    # Opus is the daily driver in every era (heavy users live on the top
    # model); Sonnet is the cheaper second, Haiku a small tail for quick
    # one-offs. The dominant slot steps up to each era's frontier Opus,
    # then to Fable 5 once it lands above Opus.
    (365, [("claude-opus-4", 7), ("claude-sonnet-4", 3)]),
    (300, [("claude-opus-4-1", 7), ("claude-sonnet-4", 3)]),
    (270, [("claude-opus-4-1", 7), ("claude-sonnet-4-5", 3)]),
    (240, [("claude-opus-4-1", 6), ("claude-sonnet-4-5", 4)]),
    (220, [("claude-opus-4-5", 8), ("claude-sonnet-4-5", 1), ("claude-haiku-4-5", 0.3)]),
    (150, [("claude-opus-4-6", 8), ("claude-sonnet-4-6", 1), ("claude-haiku-4-5", 0.3)]),
    (100, [("claude-opus-4-7", 8), ("claude-sonnet-4-6", 1), ("claude-haiku-4-5", 0.3)]),
    (40, [("claude-opus-4-8", 8), ("claude-sonnet-4-6", 1), ("claude-haiku-4-5", 0.3)]),
    # Sonnet 5 lands below the reigning Opus: a short trial window, not a
    # switch — the daily driver stays Opus 4.8 until Fable 5 ships.
    (20, [("claude-opus-4-8", 7), ("claude-sonnet-5", 2), ("claude-haiku-4-5", 0.3)]),
    # Fable 5 is the new frontier above Opus, so it takes over as driver;
    # Opus 4.8 lingers as the familiar fallback.
    (10, [("claude-fable-5", 7), ("claude-opus-4-8", 2), ("claude-haiku-4-5", 0.3)]),
    # Opus 5 succeeds 4.8 in the Opus-tier slot; Fable 5 stays the driver.
    (3, [("claude-fable-5", 7), ("claude-opus-5", 2), ("claude-haiku-4-5", 0.3)]),
]


def models_for_day(days_ago: int) -> list[tuple[str, int]]:
    era = MODEL_ERAS[0][1]
    for start, weighted in MODEL_ERAS:
        if days_ago <= start:
            era = weighted
    return era


def pick_model(rng: random.Random, days_ago: int) -> str:
    weighted = models_for_day(days_ago)
    return rng.choices([m for m, _ in weighted], weights=[w for _, w in weighted])[0]

# Servers exercised within the last 30 days → classified "active".
MCP_TOOLS_ACTIVE = [
    ("mcp__issue-tracker__create_issue", '{"title": "Follow-up"}'),
    ("mcp__issue-tracker__search_issues", '{"query": "open"}'),
    ("mcp__git-forge__open_pr", '{"branch": "smp-scheduler"}'),
    ("mcp__serial-console__read_log", '{"port": "ttyUSB0"}'),
    ("mcp__symbol-index__find_symbol", '{"symbol": "sched_pick_next"}'),
    ("mcp__perf-probe__counters", '{"event": "cache-misses"}'),
]
# Servers exercised only >30 days ago → classified "stale" (used, but idle).
MCP_TOOLS_STALE = [
    ("mcp__docs-search__query", '{"q": "retry policy"}'),
    ("mcp__trace-viewer__open", '{"file": "boot.perfetto"}'),
]

# Full lists get installed on disk (write_ecosystem); only the *_INVOKED
# slice is ever called, so the tail entries surface as installed-but-never-
# invoked zero-call rows in the Ecosystem panel.
SKILLS = ["code-review", "kernel-debug", "perf-profile", "rtl-lint",
          "crash-triage", "release-notes"]
SKILLS_INVOKED = SKILLS[:-1]  # release-notes never invoked
COMMANDS = ["optimize", "review", "bisect", "flamegraph", "defconfig", "changelog"]
COMMANDS_INVOKED = COMMANDS[:-1]  # changelog never invoked
AGENTS = ["doc-writer", "test-writer", "fuzz-harness", "bisector", "perf-analyst"]
AGENTS_INVOKED = AGENTS[:-2]  # bisector, perf-analyst never invoked

CLAUDE_VERSION = "2.0.0"


class Gen:
    def __init__(self, seed: int):
        self.rng = random.Random(seed)
        self.counter = 0

    def next_id(self, prefix: str) -> str:
        self.counter += 1
        return f"{prefix}_{self.counter:08x}"

    def uuid(self) -> str:
        h = f"{self.rng.getrandbits(128):032x}"
        return f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}"


def project_dir_name(cwd: str) -> str:
    # Mirrors ccsight's cwd_to_project_dir: every '/' becomes '-'.
    return cwd.replace("/", "-")


def base_entry(g: Gen, session_id: str, ts: datetime, cwd: str, branch: str) -> dict:
    return {
        "uuid": g.uuid(),
        "sessionId": session_id,
        "timestamp": ts.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z",
        "cwd": cwd,
        "gitBranch": branch,
        "version": CLAUDE_VERSION,
        "isSidechain": False,
    }


def usage_block(g: Gen, turn_idx: int, scale: float) -> dict:
    rng = g.rng
    # Per-turn volumes stay individually plausible (context-sized cache
    # reads); heavy daily spend comes from turn COUNT, like real usage.
    cache_read = int(min(2_000_000, 200_000 + turn_idx * rng.randint(30_000, 90_000)) * scale)
    cache_create = int(rng.randint(30_000, 250_000) * scale) if rng.random() < 0.7 else 0
    # Fresh input stays tiny next to the cached-context re-read, so the
    # cache-hit metric (cache_read / (input + cache_read)) reads near-total,
    # like real sessions where the whole prior turn is a cache hit.
    u = {
        "input_tokens": int(rng.randint(200, 1_200) * scale),
        "output_tokens": int(rng.randint(1_000, 8_000) * scale),
        "cache_read_input_tokens": cache_read,
        "cache_creation_input_tokens": cache_create,
    }
    if cache_create and rng.random() < 0.5:
        one_h = rng.randint(0, cache_create)
        u["cache_creation"] = {
            "ephemeral_5m_input_tokens": cache_create - one_h,
            "ephemeral_1h_input_tokens": one_h,
        }
    return u


def make_tool_use(g: Gen, lang: str, days_ago: int) -> tuple[dict, bool]:
    """One tool_use block + whether its result should be an error."""
    rng = g.rng
    roll = rng.random()
    # Per-draw shares are tuned so PER-SESSION adoption (>=1 hit across a
    # session's ~40 draws) lands near real usage: MCP ~25%, Skills ~4%,
    # Subagents ~48% (Skills is a rarely-touched category even for heavy
    # users; Subagents is common — real logs show a wide spread, not the
    # near-100%-for-everything a flat 6%/6%/6% split would produce).
    if roll < 0.357:
        name, inp = "Read", {"file_path": rng.choice(FILES_BY_LANG[lang])}
    elif roll < 0.595:
        name, inp = "Edit", {"file_path": rng.choice(FILES_BY_LANG[lang])}
    elif roll < 0.738:
        name, inp = "Bash", {"command": rng.choice(BASH_BY_LANG[lang])}
    elif roll < 0.833:
        name, inp = "Grep", {"pattern": "TODO", "path": "src"}
    elif roll < 0.904:
        name, inp = "Write", {"file_path": rng.choice(FILES_BY_LANG[lang])}
    elif roll < 0.976:
        name, inp = "TodoWrite", {"todos": []}
    elif roll < 0.983:
        # Recent sessions exercise only the active servers; older ones also
        # reach the stale set, so those servers' last use falls outside the
        # 30-day window and they read as stale rather than active.
        pool = MCP_TOOLS_ACTIVE if days_ago <= 30 else MCP_TOOLS_ACTIVE + MCP_TOOLS_STALE
        tool, args = rng.choice(pool)
        name, inp = tool, json.loads(args)
    elif roll < 0.984:
        name, inp = "Skill", {"skill": rng.choice(SKILLS_INVOKED)}
    else:
        name, inp = "Task", {"subagent_type": rng.choice(AGENTS_INVOKED),
                             "prompt": "survey the module"}
    is_error = rng.random() < 0.04
    block = {"type": "tool_use", "id": g.next_id("toolu"), "name": name, "input": inp}
    return block, is_error


def make_burst_tool(g: Gen, lang: str) -> tuple[dict, bool]:
    """Tool pick for work bursts — Bash-heavy, like real agentic runs."""
    rng = g.rng
    roll = rng.random()
    if roll < 0.45:
        name, inp = "Bash", {"command": rng.choice(BASH_BY_LANG[lang])}
    elif roll < 0.70:
        name, inp = "Read", {"file_path": rng.choice(FILES_BY_LANG[lang])}
    elif roll < 0.85:
        name, inp = "Edit", {"file_path": rng.choice(FILES_BY_LANG[lang])}
    elif roll < 0.93:
        name, inp = "Grep", {"pattern": "TODO", "path": "src"}
    else:
        name, inp = "Write", {"file_path": rng.choice(FILES_BY_LANG[lang])}
    is_error = rng.random() < 0.04
    return {"type": "tool_use", "id": g.next_id("toolu"), "name": name, "input": inp}, is_error


def usage_block_light(g: Gen, scale: float) -> dict:
    """Usage for a tool-only assistant turn: tiny output, cached context."""
    rng = g.rng
    return {
        "input_tokens": int(rng.randint(100, 500) * scale),
        "output_tokens": int(rng.randint(80, 400) * scale),
        "cache_read_input_tokens": int(rng.randint(150_000, 600_000) * scale),
        "cache_creation_input_tokens": 0,
    }


def build_pairs(rng: random.Random, lang: str, target: int) -> tuple[str, list]:
    """Chain arcs + followups from the archetype pool up to `target` pairs."""
    arcs = ARCS_BY_ARCH[lang]
    summary_text, first = rng.choice(arcs)
    pairs = list(first)
    while len(pairs) < target:
        _, extra = rng.choice(arcs)
        pairs += list(extra)
        pairs += rng.sample(FOLLOWUPS, k=rng.randint(0, 2))
    return summary_text, pairs[:target]


def emit_pairs(g: Gen, lines: list, session_id: str, cwd: str, branch: str,
               lang: str, days_ago: int, day_scale: float, pairs: list,
               ts: datetime, ts_cap: datetime | None, sidechain: bool,
               model: str) -> tuple[datetime, str]:
    """Append user/assistant/tool entries for `pairs` starting at `ts`."""
    rng = g.rng

    def advance(lo: int, hi: int) -> None:
        # Cap today's sessions at "now" — an uncapped turn walk stamps
        # timestamps in the future, which the Live tab renders as end
        # times ahead of the wall clock.
        nonlocal ts
        ts += timedelta(seconds=rng.randint(lo, hi))
        if ts_cap is not None and ts > ts_cap:
            ts = ts_cap

    for turn, (user_text, ai_text) in enumerate(pairs):
        user = base_entry(g, session_id, ts, cwd, branch)
        user["type"] = "user"
        user["isSidechain"] = sidechain
        text = user_text
        # Occasional slash command — surfaces in the Commands section.
        # Tuned for ~27% session adoption (real-data rate) over a session's
        # ~20 turns, not the near-guaranteed hit a higher per-turn rate gives.
        if not sidechain and turn > 0 and rng.random() < 0.016:
            cmd = rng.choice(COMMANDS_INVOKED)
            text = f"<command-name>/{cmd}</command-name>\n{text}"
        user["message"] = {"role": "user", "content": text}
        lines.append(user)
        advance(5, 40)

        # Rare mid-session model switch (badge must show the LAST model).
        if turn > 0 and rng.random() < 0.05:
            model = pick_model(rng, days_ago)

        tool_uses = []
        results = []
        for _ in range(rng.randint(0, 4)):
            block, is_err = make_tool_use(g, lang, days_ago)
            tool_uses.append(block)
            results.append((block["id"], is_err))

        asst = base_entry(g, session_id, ts, cwd, branch)
        asst["type"] = "assistant"
        asst["isSidechain"] = sidechain
        asst["requestId"] = g.next_id("req")
        content = [{"type": "text", "text": ai_text}] + tool_uses
        asst["message"] = {
            "id": g.next_id("msg"),
            "role": "assistant",
            "model": model,
            "content": content,
            "usage": usage_block(g, turn, day_scale),
        }
        lines.append(asst)
        advance(3, 30)

        for tool_id, is_err in results:
            res = base_entry(g, session_id, ts, cwd, branch)
            res["type"] = "user"
            res["isSidechain"] = sidechain
            res["message"] = {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_id,
                    "content": "command not found: expected binary" if is_err else "ok",
                    "is_error": is_err,
                }],
            }
            lines.append(res)
            advance(2, 20)

        # Work burst: a run of consecutive tool-only exchanges. The conv
        # pane folds these into one "⚙ N tools" group row — the visual
        # signature of heavy agentic work. Frequency/length is tuned so the
        # tool-only share of assistant messages lands near real usage
        # (~40-50%), not the 65-84% a more frequent/longer burst produced —
        # real sessions stay text-forward even when tool-heavy.
        if not sidechain and rng.random() < 0.13:
            for _ in range(int(rng.triangular(2, 12, 4))):
                block, is_err = make_burst_tool(g, lang)
                t_asst = base_entry(g, session_id, ts, cwd, branch)
                t_asst["type"] = "assistant"
                t_asst["isSidechain"] = sidechain
                t_asst["requestId"] = g.next_id("req")
                t_asst["message"] = {
                    "id": g.next_id("msg"),
                    "role": "assistant",
                    "model": model,
                    "content": [block],
                    "usage": usage_block_light(g, day_scale),
                }
                lines.append(t_asst)
                advance(2, 15)
                t_res = base_entry(g, session_id, ts, cwd, branch)
                t_res["type"] = "user"
                t_res["isSidechain"] = sidechain
                t_res["message"] = {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": block["id"],
                        "content": "command not found: expected binary" if is_err else "ok",
                        "is_error": is_err,
                    }],
                }
                lines.append(t_res)
                advance(2, 15)

        advance(20, 240)

    return ts, model


def write_session(g: Gen, path: Path, session_id: str, start: datetime, cwd: str,
                  branch: str, lang: str, days_ago: int, day_scale: float,
                  ts_cap: datetime | None = None,
                  sidechain: bool = False) -> datetime:
    """Write one session JSONL; returns the last entry timestamp."""
    rng = g.rng
    lines: list[dict] = []
    model = pick_model(rng, days_ago)

    # Turn count is a skewed long tail: most sessions are short (a quick
    # question or edit), a few run long — matching how real usage looks and
    # keeping work-tokens-per-session realistic rather than uniformly huge.
    # The floor is high enough that a session's own turns usually bridge
    # into the next hour bucket, so idle time between DIFFERENT sessions
    # (not gaps inside one) is what shows as a gap in the hourly Activity
    # bar — not an artifact of a session finishing within a few minutes.
    if sidechain:
        target = int(rng.triangular(3, 26, 10))
    else:
        target = int(rng.triangular(3, 48, 10))
    summary_text, pairs = build_pairs(rng, lang, target)

    _, _ = emit_pairs(g, lines, session_id, cwd, branch, lang, days_ago,
                      day_scale, pairs, start, ts_cap, sidechain, model)

    if not sidechain and rng.random() < 0.4:
        lines.append({
            "type": "summary",
            "summary": summary_text,
            "leafUuid": g.uuid(),
        })
    if not sidechain and rng.random() < 0.3:
        lines.append({
            "type": "custom-title",
            "customTitle": rng.choice(CUSTOM_TITLES_BY_ARCH[lang]),
            "sessionId": session_id,
        })

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        for line in lines:
            f.write(json.dumps(line, separators=(",", ":")) + "\n")
    last = next((e["timestamp"] for e in reversed(lines) if "timestamp" in e), None)
    if last is None:
        return start
    return datetime.strptime(last, "%Y-%m-%dT%H:%M:%S.%f%z") if "+" in last else \
        datetime.strptime(last.replace("Z", "+0000"), "%Y-%m-%dT%H:%M:%S.%f%z")


def write_marathon(g: Gen, path: Path, session_id: str, cwd: str, branch: str,
                   lang: str, start_days_ago: int, end_days_ago: int,
                   active_prob: float, now: datetime, local_tz,
                   hour_weights: list) -> datetime:
    """A multi-week session resumed across many days (renders as `»` and a
    wide Live time range) — the shape a heavy user's flagship work takes."""
    rng = g.rng
    lines: list[dict] = []
    model = pick_model(rng, start_days_ago)
    last_ts = now
    for day_offset in range(start_days_ago, end_days_ago - 1, -1):
        if day_offset != end_days_ago and rng.random() > active_prob:
            continue
        day = (now.astimezone(local_tz) - timedelta(days=day_offset)).replace(microsecond=0)
        hour = rng.choices(range(24), weights=hour_weights)[0]
        start_local = day.replace(hour=hour, minute=rng.randint(0, 59), second=0)
        start = start_local.astimezone(timezone.utc)
        if day_offset == 0 and start > now:
            start = now - timedelta(minutes=rng.randint(20, 120))
        # Same floor-raising logic as write_session's target above — a
        # marathon's daily visit should span more than one hour bucket.
        _, pairs = build_pairs(rng, lang, int(rng.triangular(6, 32, 14)))
        last_ts, model = emit_pairs(
            g, lines, session_id, cwd, branch, lang, day_offset, 1.0, pairs,
            start, now if day_offset == 0 else None, False, model)
    lines.append({
        "type": "custom-title",
        "customTitle": rng.choice(CUSTOM_TITLES_BY_ARCH[lang]),
        "sessionId": session_id,
    })
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        for line in lines:
            f.write(json.dumps(line, separators=(",", ":")) + "\n")
    return last_ts


def write_aborted(g: Gen, path: Path, session_id: str, ts: datetime, cwd: str,
                  branch: str) -> None:
    """A dead-on-arrival session: one user line, no assistant reply. Renders
    as the `0 $0 [?]` rows every real dataset accumulates."""
    entry = base_entry(g, session_id, ts, cwd, branch)
    entry["type"] = "user"
    entry["message"] = {
        "role": "user",
        "content": g.rng.choice(["?", "never mind — wrong window", "hold on, wrong repo"]),
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(entry, separators=(",", ":")) + "\n")


def write_ecosystem(home: Path) -> None:
    """Config surfaces: MCP servers, skills, commands, agents.

    Includes configured-but-never-invoked entries in every category so the
    Ecosystem panel's zero-call rows have something to show.
    """
    claude = home / ".claude"
    (home / ".claude.json").write_text(json.dumps({
        "mcpServers": {
            # Active: exercised within the last 30 days (see MCP_TOOLS_ACTIVE).
            "issue-tracker": {"command": "issue-tracker-mcp"},
            "git-forge": {"command": "git-forge-mcp"},
            "serial-console": {"command": "serial-console-mcp"},
            "symbol-index": {"command": "symbol-index-mcp"},
            "perf-probe": {"command": "perf-probe-mcp"},
            # Stale: used, but only >30 days ago (see MCP_TOOLS_STALE).
            "docs-search": {"command": "docs-search-mcp"},
            "trace-viewer": {"command": "trace-viewer-mcp"},
            # Configured but never invoked.
            "scratchpad": {"command": "scratchpad-mcp"},
            "fpga-flasher": {"command": "fpga-flasher-mcp"},
        }
    }, indent=2))
    # cleanupPeriodDays silences the retention-warning banner in captures.
    (claude / "settings.json").write_text(json.dumps({"cleanupPeriodDays": 36500}) + "\n")
    for skill in SKILLS:
        d = claude / "skills" / skill
        d.mkdir(parents=True, exist_ok=True)
        (d / "SKILL.md").write_text(f"# {skill}\n\nDemo placeholder skill.\n")
    (claude / "commands").mkdir(parents=True, exist_ok=True)
    for cmd in COMMANDS:
        (claude / "commands" / f"{cmd}.md").write_text(f"Demo placeholder command: /{cmd}\n")
    (claude / "agents").mkdir(parents=True, exist_ok=True)
    for agent in AGENTS:
        (claude / "agents" / f"{agent}.md").write_text(f"# {agent}\n\nDemo placeholder agent.\n")


def write_live_meta(home: Path, pid: int, session_id: str, cwd: str,
                    now: datetime, status: str = "running") -> None:
    d = home / ".claude" / "sessions"
    d.mkdir(parents=True, exist_ok=True)
    ms = int(now.timestamp() * 1000)
    # Vary the start so the Live rows show a range of elapsed times, not one
    # uniform duration. Deterministic in the pid, so runs stay reproducible.
    started_min = 15 + pid % 180
    (d / f"{pid}.json").write_text(json.dumps({
        "pid": pid,
        "sessionId": session_id,
        "cwd": cwd,
        "name": "demo session",
        "status": status,
        "startedAt": ms - started_min * 60 * 1000,
        "updatedAt": ms - (pid % 5) * 60 * 1000,
    }))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--home", required=True, help="demo HOME dir to populate (never your real home)")
    ap.add_argument("--days", type=int, default=90, help="history span in days")
    ap.add_argument("--seed", type=int, default=42, help="RNG seed (deterministic output)")
    ap.add_argument("--force", action="store_true", help="allow writing into a non-empty dir")
    ap.add_argument("--live-pid", type=int, action="append", default=[],
                    help="alive PID to register as a busy live session (repeatable)")
    args = ap.parse_args()

    home = Path(args.home).resolve()
    real_home = Path(os.path.expanduser("~")).resolve()
    if home == real_home:
        print(f"refusing to write into the real home: {home}", file=sys.stderr)
        return 1
    if (home / ".claude").exists():
        if not args.force:
            print(f"{home}/.claude already exists; pass --force to overwrite", file=sys.stderr)
            return 1
        # Regeneration must start clean: filenames are RNG-derived, so stale
        # files from a previous run would otherwise accumulate alongside.
        shutil.rmtree(home / ".claude")
    # Drop ccsight state too, or the tantivy index keeps deleted sessions.
    if (home / ".ccsight").exists():
        shutil.rmtree(home / ".ccsight")

    g = Gen(args.seed)
    rng = g.rng
    now = datetime.now(timezone.utc)
    # ccsight buckets the Hourly / heatmap graphs by LOCAL time, so session
    # hours are picked on a local wall clock (a developer's workday) and only
    # converted to UTC for the on-disk `Z` timestamp. Picking hours in UTC
    # would shift the whole workday by the viewer's local offset, landing
    # the daytime peak at the wrong hours.
    local_tz = now.astimezone().tzinfo
    projects_root = home / ".claude" / "projects"

    n_sessions = 0
    today_sessions: list[tuple[str, str]] = []  # (session_id, cwd) for --live-pid

    # Month-level intensity: per-day noise averages out across a month of
    # active days, landing every month near the same total. A shared per-month
    # multiplier makes some months crunch-heavy and others quiet, so the
    # monthly bars read jagged like real spend.
    month_scales: dict[tuple[int, int], float] = {}

    for day_offset in range(args.days, -1, -1):
        day = (now.astimezone(local_tz) - timedelta(days=day_offset)).replace(microsecond=0)
        weekday = day.weekday()
        # Weekends lighter; some weekdays skipped for a realistic heatmap.
        p_active = 0.5 if weekday >= 5 else 0.95
        if day_offset != 0 and rng.random() > p_active:
            continue
        month_scale = month_scales.setdefault((day.year, day.month), rng.uniform(0.4, 2.3))
        # Day-level intensity: spend fluctuates around the month's mean (light
        # days to crunch days) instead of every day costing the same.
        day_scale = rng.uniform(0.4, 1.2) * month_scale
        n_day = rng.randint(3, 10) if weekday >= 5 else rng.randint(10, 24)
        # Today mirrors a real heavy day: a handful of NEW sessions — the
        # bulk of today's tokens comes from the resumed marathons below.
        if day_offset == 0:
            n_day = rng.randint(3, 6)
        for _ in range(n_day):
            cwd, _, lang, branches = rng.choices(PROJECTS, weights=[p[1] for p in PROJECTS])[0]
            branch = rng.choice(branches)
            session_id = g.uuid()
            hour = rng.choices(range(24), weights=HOUR_WEIGHTS)[0]
            start_local = day.replace(hour=hour, minute=rng.randint(0, 59),
                                      second=rng.randint(0, 59), microsecond=0)
            start = start_local.astimezone(timezone.utc)
            if day_offset == 0 and start > now:
                start = now - timedelta(minutes=rng.randint(10, 120))
            path = projects_root / project_dir_name(cwd) / f"{session_id}.jsonl"
            last_ts = write_session(g, path, session_id, start, cwd, branch, lang,
                                    day_offset, day_scale,
                                    ts_cap=now if day_offset == 0 else None)
            epoch = min(last_ts, now).timestamp()
            os.utime(path, (epoch, epoch))
            n_sessions += 1
            if day_offset == 0:
                today_sessions.append((session_id, cwd))

            # Subagent artifacts next to their parent session. Heavy agentic
            # use fans out into several substantial child sessions, so the
            # subagent token share stays a meaningful slice of the total.
            if rng.random() < 0.35:
                for _ in range(rng.randint(1, 4)):
                    sub_id = g.uuid()
                    sub_path = projects_root / project_dir_name(cwd) / f"agent-{sub_id}.jsonl"
                    sub_last = write_session(g, sub_path, sub_id, start, cwd, branch, lang,
                                             day_offset, day_scale,
                                             ts_cap=now if day_offset == 0 else None,
                                             sidechain=True)
                    sub_epoch = min(sub_last, now).timestamp()
                    os.utime(sub_path, (sub_epoch, sub_epoch))
                    n_sessions += 1

        # Dead-on-arrival sessions: real datasets accumulate `0 $0 [?]` rows
        # (a question typed into the wrong window, an instant abort).
        if rng.random() < 0.06:
            ab_id = g.uuid()
            ab_cwd, _, _, ab_branches = rng.choices(PROJECTS, weights=[p[1] for p in PROJECTS])[0]
            ab_ts = day.replace(hour=rng.choices(range(24), weights=HOUR_WEIGHTS)[0],
                                minute=rng.randint(0, 59)).astimezone(timezone.utc)
            if ab_ts > now:
                ab_ts = now - timedelta(minutes=rng.randint(5, 60))
            ab_path = projects_root / project_dir_name(ab_cwd) / f"{ab_id}.jsonl"
            write_aborted(g, ab_path, ab_id, ab_ts, ab_cwd, rng.choice(ab_branches))
            os.utime(ab_path, (ab_ts.timestamp(), ab_ts.timestamp()))
            n_sessions += 1

    # Marathon sessions: multi-week flagship work resumed every few days.
    # These carry the `»` continued glyph, dominate the Live tab with wide
    # time ranges, and give "today" its continued rows — the signature shape
    # of heavy real usage. (start_days_ago, end_days_ago, active_prob, cwd).
    marathon_specs = [
        (min(args.days, 90), 0, 0.45, "/home/dev/src/kernel", "os"),
        (min(args.days, 35), 0, 0.50, "/home/dev/src/hypervisor", "vmm"),
        (min(args.days, 21), 2, 0.50, "/home/dev/src/firmware", "fw"),
        (min(args.days, 14), 1, 0.60, "/home/dev/src/toolchain", "tc"),
        (min(args.days, 10), 0, 0.60, "/home/dev/src/drivers", "drv"),
    ]
    marathons_today: list[tuple[str, str, Path]] = []
    marathons_older: list[tuple[str, str, Path]] = []
    for start_ago, end_ago, prob, m_cwd, m_lang in marathon_specs:
        m_id = g.uuid()
        m_branch = next(b for c, _, _, bs in PROJECTS if c == m_cwd for b in bs[1:2])
        m_path = projects_root / project_dir_name(m_cwd) / f"{m_id}.jsonl"
        m_last = write_marathon(g, m_path, m_id, m_cwd, m_branch, m_lang,
                                start_ago, end_ago, prob, now, local_tz, HOUR_WEIGHTS)
        epoch = min(m_last, now).timestamp()
        os.utime(m_path, (epoch, epoch))
        n_sessions += 1
        (marathons_today if end_ago == 0 else marathons_older).append((m_id, m_cwd, m_path))

    write_ecosystem(home)

    # Live PID assignment mirrors a real Active set: one BUSY marathon
    # (writing right now), a couple of today sessions, and older marathons
    # whose processes are still alive from days ago.
    assignments: list[tuple[str, str, str, Path | None]] = []
    if marathons_today:
        m_id, m_cwd, m_path = marathons_today[0]
        assignments.append(("busy", m_id, m_cwd, m_path))
    for session_id, cwd in today_sessions[:2]:
        assignments.append(("running", session_id, cwd, None))
    for m_id, m_cwd, m_path in marathons_older[:2]:
        assignments.append(("running", m_id, m_cwd, m_path))
    for pid, (status, session_id, cwd, m_path) in zip(args.live_pid, assignments):
        write_live_meta(home, pid, session_id, cwd, now, status=status)
        if status == "busy" and m_path is not None:
            # A busy session's transcript is being written right now.
            os.utime(m_path, (now.timestamp(), now.timestamp()))

    print(f"demo home: {home}")
    print(f"sessions written: {n_sessions} across {len(PROJECTS)} projects, ~{args.days} days")
    print(f"launch with: HOME={home} target/release/ccsight")
    return 0


if __name__ == "__main__":
    sys.exit(main())
