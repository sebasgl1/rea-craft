//! Regressions for argument loads and outgoing stack arguments that pseudo-C folds into a call:
//! a memory write that may change the bytes they read keeps them in place.

use craft_anything_disasm::pseudo::lift;
use craft_anything_disasm::{Image, Limits, NoControl, Program};
use craft_anything_formats::{Arch, Binary};
use craft_anything_testkit as tk;
use std::sync::Arc;

/// The pseudo-C of the function at 0x1000 in a raw image.
fn lift_raw(arch: Arch, code: Vec<u8>) -> String {
    let mut bin = Binary::raw(arch, 0x1000, code.len() as u64, 0);
    bin.entry = Some(0x1000);
    let img = Image::from_binary(Arc::from(code), bin);
    let prog = Program::analyze(&img, &Limits { scan_prologues: false, ..Limits::default() }, &NoControl).expect("analyze");
    lift(&img, &prog, prog.functions.get(&0x1000).expect("function at 0x1000"), &|_| None).text
}

/// The trimmed line of `text` that calls `callee`.
fn call_line(text: &str, callee: &str) -> String {
    text.lines().find(|l| l.contains(&format!("{callee}("))).unwrap_or_default().trim().to_string()
}

/// AArch64 `ldr x0, [x19, #8]; <store>; bl 0x1010`: `stp x1, x2, [x19]` writes 16 bytes and so
/// reaches the load's, so the load stays before it; `str x1, [x19]` writes 8 bytes that end where
/// the load starts, so the load still folds into the call. The decoder sizes neither access.
#[test]
fn a_pair_store_keeps_an_a64_argument_load_and_a_disjoint_store_does_not() {
    for (store, call) in [(0xa900_0a61u32, "sub_1010(x0);"), (0xf900_0261, "sub_1010(*(void*)(x19 + 8));")] {
        let words = [0xf940_0660u32, store, 0x9400_0002, 0xd65f_03c0, 0xd65f_03c0];
        let text = lift_raw(Arch::Aarch64, words.iter().flat_map(|w| w.to_le_bytes()).collect());
        assert_eq!(call_line(&text, "sub_1010"), call, "{store:#x}:\n{text}");
        assert_eq!(text.contains("x0 = *(void*)(x19 + 8);"), call == "sub_1010(x0);", "{store:#x}:\n{text}");
    }
}

/// AArch64 `ldr x0, 0x1018` (a literal, named by address rather than as a memory operand); then
/// `str x1, [x2]`, which may write the literal, keeps the load in place, while `str x1, [sp, #8]`,
/// a frame slot, lets it fold.
#[test]
fn an_a64_literal_load_stays_before_a_store_that_may_write_it() {
    for (store, kept) in [(0xf900_0041u32, true), (0xf900_07e1, false)] {
        let words = [0x5800_00c0u32, store, 0x9400_0002, 0xd65f_03c0, 0xd65f_03c0, 0xd503_201f, 0, 0];
        let text = lift_raw(Arch::Aarch64, words.iter().flat_map(|w| w.to_le_bytes()).collect());
        assert_eq!(call_line(&text, "sub_1010") == "sub_1010(x0);", kept, "{store:#x}:\n{text}");
        assert_eq!(text.lines().any(|l| l.trim_start().starts_with("x0 = ")), kept, "{store:#x}:\n{text}");
    }
}

/// x86-64 `mov rdi, <load>; <store>; call 0x1020` (each padded with `nop`s): a load through `rbp`
/// with an index may read what a RIP-relative store writes, and `fs:[rax]` what `[rax+8]` writes;
/// an unindexed frame slot and an unsegmented `[rax]` still fold.
#[test]
fn indexed_and_segment_loads_stay_before_a_store() {
    let rip_store = [0x48, 0x89, 0x15, 0x00, 0x01, 0x00, 0x00]; // mov [rip+0x100], rdx
    let rax8_store = [0x48, 0x89, 0x50, 0x08, 0x90, 0x90, 0x90]; // mov [rax+8], rdx
    for (load, store, call) in [
        ([0x48u8, 0x8b, 0x7c, 0x05, 0x00], rip_store, "sub_1020(rdi);"), // mov rdi, [rbp+rax]
        ([0x48, 0x8b, 0x7d, 0x00, 0x90], rip_store, "sub_1020(arg_0);"), // mov rdi, [rbp]
        ([0x64, 0x48, 0x8b, 0x38, 0x90], rax8_store, "sub_1020(rdi);"),  // mov rdi, fs:[rax]
        ([0x48, 0x8b, 0x38, 0x90, 0x90], rax8_store, "sub_1020(*(uint64_t*)(rax));"), // mov rdi, [rax]
    ] {
        let mut code = [&load[..], &store, &[0xe8, 0x0f, 0x00, 0x00, 0x00, 0xc3]].concat();
        code.resize(0x20, 0xcc);
        code.push(0xc3);
        let text = lift_raw(Arch::X86_64, code);
        assert_eq!(call_line(&text, "sub_1020"), call, "{load:x?}:\n{text}");
        assert_eq!(text.contains("rdi = "), call == "sub_1020(rdi);", "{load:x?}:\n{text}");
    }
}

/// Win64: `mov [rsp+0x20], rax; inc qword [rsp+0x20]` before an import call passes the slot as it
/// is at the call and keeps the store that set it.
#[test]
fn a_write_to_an_outgoing_stack_slot_stops_it_folding() {
    let mut pe = tk::pe64();
    let code: [u8; 47] = [
        0x48, 0x83, 0xec, 0x68, // sub rsp, 0x68
        0x48, 0x89, 0x44, 0x24, 0x20, // mov [rsp+0x20], rax
        0x48, 0xff, 0x44, 0x24, 0x20, // inc qword [rsp+0x20]
        0x41, 0xb9, 0x04, 0x00, 0x00, 0x00, // mov r9d, 4
        0x41, 0xb8, 0x03, 0x00, 0x00, 0x00, // mov r8d, 3
        0xba, 0x02, 0x00, 0x00, 0x00, // mov edx, 2
        0xb9, 0x01, 0x00, 0x00, 0x00, // mov ecx, 1
        0xff, 0x15, 0x36, 0x11, 0x00, 0x00, // call [rip+0x1136] (the puts IAT slot)
        0x48, 0x83, 0xc4, 0x68, // add rsp, 0x68
        0xc3, // ret
    ];
    pe[0x400..0x400 + code.len()].copy_from_slice(&code);
    let img = Image::parse(Arc::from(pe)).expect("parse PE64");
    let prog = Program::analyze(&img, &Limits::default(), &NoControl).expect("analyze PE64");
    let text = lift(&img, &prog, prog.functions.get(&(tk::PE_BASE + tk::PE_TEXT_RVA)).expect("main"), &|_| None).text;
    assert_eq!(call_line(&text, "puts"), "puts(1, 2, 3, 4, stack_20);", "{text}");
    assert!(text.contains("stack_20 = rax;") && text.contains("stack_20++;"), "{text}");
}
