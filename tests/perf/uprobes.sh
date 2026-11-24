#!/bin/bash
# ADR-33 T6: wall-clock timing of the cut's steps with uprobes on the lunixd binary (root; source this file).
#   up_setup BIN     register entry and return probes (group lunixt6) on the functions below, found with nm in BIN
#   up_start         clear the trace buffer and enable the probes
#   up_stop FILE     disable them and save the trace to FILE
#   up_clear         remove the probes
# Functions: cut_and_commit (a whole cut C1-C9), Packer::finish (end of C4: the destage stage and its last seal), Packer::seal_current
# and ContainerWriter::seal (segment seals, each with its fdatasync), FileJournal::reclaim (C9) and zero_range (the zero rewrite inside
# it), Gc::urgent and Engine::run_urgent (the urgent GC pass under the coordinator), true_up_index (C4). C5-C9 is the time from the return
# of Packer::finish to the return of cut_and_commit. bench/phase1/perf/shares.py --uprobes pairs the events.
TRACEFS=/sys/kernel/tracing
[ -d "$TRACEFS" ] || TRACEFS=/sys/kernel/debug/tracing
up_fn() { # name, nm pattern (anchored at the end), kinds
  local name=$1 pat=$2 addr off
  addr=$(nm -C "$UP_BIN" 2>/dev/null | grep -E " [tT] " | grep -E -- "$pat\$" | head -1 | cut -d' ' -f1)
  [ -n "$addr" ] || { echo "uprobes: no symbol for $name" >&2; return 1; }
  # file offset of the address: vaddr - segment vaddr + segment offset (executable PT_LOAD)
  off=$(readelf -lW "$UP_BIN" | awk -v a="0x$addr" 'function h(x){return strtonum(x)} $1=="LOAD" && $0 ~ / E / && h(a)>=h($3) && h(a)<h($3)+h($6) {printf "0x%x", h(a)-h($3)+h($2); exit}')
  [ -n "$off" ] || { echo "uprobes: no offset for $name" >&2; return 1; }
  echo "p:lunixt6/$name $UP_BIN:$off" >> "$TRACEFS/uprobe_events"
  echo "r:lunixt6/${name}_r $UP_BIN:$off" >> "$TRACEFS/uprobe_events"
}
up_setup() {
  UP_BIN=$1
  up_clear
  up_fn cut ' <lunix_engine::engine::Inner>::cut_and_commit'
  up_fn finish ' <lunix_destage::pack::Packer>::finish'
  up_fn segseal ' <lunix_destage::pack::Packer>::seal_current'
  up_fn cwseal ' <lunix_store::container::ContainerWriter>::seal'
  up_fn reclaim ' <lunix_journal::journal::FileJournal as lunix_iface::journal::Journal>::reclaim'
  up_fn zero ' lunix_journal::sys::zero_range'
  up_fn gcurgent ' <lunix_gc::collector::Gc as lunix_iface::gc::Collector>::urgent'
  up_fn runurgent ' <lunix_engine::engine::Engine as lunix_iface::gc::GcControl>::run_urgent'
  up_fn trueup ' <lunix_engine::engine::Inner>::true_up_index'
}
up_start() { echo 0 > "$TRACEFS/tracing_on"; : > "$TRACEFS/trace"; echo 1 > "$TRACEFS/events/lunixt6/enable"; echo 1 > "$TRACEFS/tracing_on"; }
up_off() { [ -e "$TRACEFS/events/lunixt6/enable" ] && echo 0 > "$TRACEFS/events/lunixt6/enable"; true; }
up_stop() { up_off; cat "$TRACEFS/trace" > "$1" 2>/dev/null; }
up_clear() { up_off; echo > "$TRACEFS/uprobe_events" 2>/dev/null; true; }
