#!/usr/bin/env python3
from __future__ import annotations

import argparse
import ast
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Arg:
    ty: str
    name: str | None
    has_default: bool


@dataclass(frozen=True)
class Signature:
    args: list[Arg]


class Syntax:
    def __init__(self) -> None:
        self.parens = 0
        self.brackets = 0
        self.braces = 0
        self.quote: str | None = None
        self.escaped = False

    def top_level(self) -> bool:
        return (
            self.parens == 0
            and self.brackets == 0
            and self.braces == 0
            and self.quote is None
        )

    def push(self, ch: str) -> None:
        if self.quote is not None:
            if self.escaped:
                self.escaped = False
            elif ch == "\\":
                self.escaped = True
            elif ch == self.quote:
                self.quote = None
            return

        if ch == "'" or ch == '"':
            self.quote = ch
        elif ch == "(":
            self.parens += 1
        elif ch == ")":
            self.parens -= 1
        elif ch == "[":
            self.brackets += 1
        elif ch == "]":
            self.brackets -= 1
        elif ch == "{":
            self.braces += 1
        elif ch == "}":
            self.braces -= 1


def matching(raw: str, open_index: int, open_ch: str = "(", close_ch: str = ")") -> int:
    syntax = Syntax()
    depth = 0
    for index, ch in enumerate(raw[open_index:], start=open_index):
        if syntax.quote is None:
            if ch == open_ch:
                depth += 1
            elif ch == close_ch:
                depth -= 1
                if depth == 0:
                    return index
        syntax.push(ch)
    raise ValueError(f"missing {close_ch!r} for {open_ch!r} at offset {open_index}")


def invocation_list(raw: str, prefix: str) -> list[str]:
    return [
        wrapped(part[len(prefix) - 1 :]) for part in split_top_level(raw, ",") if part
    ]


def split_top_level(raw: str, delimiter: str) -> list[str]:
    parts = []
    start = 0
    syntax = Syntax()
    for index, ch in enumerate(raw):
        if syntax.top_level() and ch == delimiter:
            parts.append(raw[start:index].strip())
            start = index + 1
        syntax.push(ch)
    parts.append(raw[start:].strip())
    return parts


def wrapped(raw: str, open_ch: str = "(", close_ch: str = ")") -> str:
    raw = raw.strip()
    close = matching(raw, 0, open_ch, close_ch)
    return raw[len(open_ch) : close]


def kw(raw: str, key: str) -> str:
    marker = f"{key}="
    start = raw.index(marker)
    value = raw[start + len(marker) :]
    syntax = Syntax()
    for index, ch in enumerate(value):
        if syntax.top_level() and ch == ",":
            return value[:index].strip()
        syntax.push(ch)
    return value.strip()


def py_string(raw: str) -> str:
    return ast.literal_eval(raw.strip())


def parse_supported_funcs(text: str) -> dict[tuple[str, str], list[Signature]]:
    funcs: dict[tuple[str, str], list[Signature]] = {}
    for line in text.splitlines():
        receiver_raw, signatures_raw = split_top_level(wrapped(line), ",")
        prefix, kind, field = next(
            (prefix, kind, field)
            for prefix, kind, field in [
                ("SupportedMethod(", "Method", "method"),
                ("SupportedBuiltin(", "Builtin", "name"),
                ("SupportedAttribute(", "Attribute", "attr"),
            ]
            if receiver_raw.startswith(prefix)
        )
        close = matching(receiver_raw, len(prefix) - 1)
        receiver = kind, py_string(kw(receiver_raw[len(prefix) : close], field))

        for signature_text in invocation_list(
            wrapped(signatures_raw, "[", "]"), "SupportedSignature("
        ):
            args_raw = kw(signature_text, "args")
            args_raw = wrapped(args_raw)

            args = []
            for arg_text in invocation_list(args_raw, "SupportedFuncArg("):
                raw_ty = kw(arg_text, "ty")
                ty = parse_ty(raw_ty)

                raw_name = kw(arg_text, "argument_name")
                name = None
                if raw_name != "None":
                    name = py_string(raw_name)

                raw_default = kw(arg_text, "default")
                args.append(Arg(ty, name, raw_default != "None"))

            funcs.setdefault(receiver, []).append(Signature(args))
    return funcs


def parse_ty(raw: str) -> str:
    raw = raw.strip()
    if raw.startswith("SubClassOf("):
        open_index = raw.find("(")
        close = matching(raw, open_index)
        body = raw[open_index + 1 : close]
        ty_class = kw(body, "ty_class")
        ty_name = ty_class.rsplit(".", 1)[-1]
        ty_name = ty_name[:-2] if ty_name.endswith("'>") else ty_class
        return (
            "SupportedTy::SubClassOf { "
            f"ty_name: {rs(ty_name)}.to_string(), "
            f"match_nullable: {rb(kw(body, 'match_nullable') == 'True')} "
            "}"
        )

    open_index = raw.index("(")
    ty_name = raw[:open_index]
    close = matching(raw, open_index)
    body = raw[open_index + 1 : close]
    nullable = kw(body, "nullable") == "True"

    if ty_name in {
        "TyAny",
        "TyBool",
        "TyBytes",
        "TyDate",
        "TyDateTime",
        "TyFloat",
        "TyHashlibHash",
        "TyInt",
        "TyJson",
        "TyNone",
        "TyReMatch",
        "TyRePattern",
        "TyRequestsHttpResponse",
        "TySequenceMatcher",
        "TyStr",
        "TyTime",
        "TyTimedelta",
        "TyTimeZone",
    }:
        variant = ty_name[2:]
        return f"SupportedTy::{variant} {{ nullable: {rb(nullable)} }}"

    if ty_name == "TyClass":
        raw_module = kw(body, "module")
        raw_name = kw(body, "name")
        module = py_string(raw_module)
        name = py_string(raw_name)
        return (
            "SupportedTy::Class { "
            f"nullable: {rb(nullable)}, "
            f"module: {rs(module)}.to_string(), "
            f"name: {rs(name)}.to_string() "
            "}"
        )

    if ty_name in {
        "TyCounter",
        "TyFrozenSet",
        "TyGenerator",
        "TyIterable",
        "TyList",
        "TySet",
    }:
        raw_items = kw(body, "items")
        items = parse_ty(raw_items)
        variant = "FrozenSet" if ty_name == "TyFrozenSet" else ty_name[2:]
        return (
            f"SupportedTy::{variant} {{ "
            f"nullable: {rb(nullable)}, "
            f"items: Box::new({items}) "
            "}"
        )

    if ty_name == "TyDict":
        raw_key = kw(body, "key_type")
        raw_value = kw(body, "value_type")
        key_type = parse_ty(raw_key)
        value_type = parse_ty(raw_value)
        return (
            "SupportedTy::Dict { "
            f"nullable: {rb(nullable)}, "
            f"key_type: Box::new({key_type}), "
            f"value_type: Box::new({value_type}) "
            "}"
        )

    if ty_name == "TyModule":
        raw_name = kw(body, "name")
        name = py_string(raw_name)
        return (
            "SupportedTy::Module { "
            f"nullable: {rb(nullable)}, "
            f"name: {rs(name)}.to_string() "
            "}"
        )

    if ty_name == "TyTuple":
        raw_items = kw(body, "items")
        items_raw = wrapped(raw_items)
        items = []
        for item in split_top_level(items_raw, ","):
            if item:
                items.append(parse_ty(item))
        return (
            "SupportedTy::Tuple { "
            f"nullable: {rb(nullable)}, "
            f"items: vec![{', '.join(items)}], "
            f"is_variable: {rb(kw(body, 'is_variable') == 'True')} "
            "}"
        )

    return (
        "SupportedTy::Other { "
        f"nullable: {rb(nullable)}, "
        f"name: {rs(ty_name)}.to_string() "
        "}"
    )


def render(funcs: dict[tuple[str, str], list[Signature]], source: Path) -> str:
    lines = [
        "// Generated by scripts/generate_supported_funcs.py. Do not edit by hand.",
        f"// Source: {source.name}",
        "// Regenerate from `crates/ty_chalk` with:",
        "// `uv run --no-project python scripts/generate_supported_funcs.py scripts/SUPPORTED_FUNCS.data -o src/supported_functions/current_snapshot.rs`",
        "",
        "use std::collections::BTreeMap;",
        "",
        "use super::{CallKind, SupportedArg, SupportedCall, SupportedFuncs, SupportedSignature, SupportedTy};",
        "",
        "pub(super) fn supported_funcs() -> SupportedFuncs {",
        "    let mut impls: BTreeMap<SupportedCall, Vec<SupportedSignature>> = BTreeMap::new();",
    ]
    for (kind, name), signatures in funcs.items():
        lines.extend(
            [
                "    impls.insert(",
                f"        supported_call(CallKind::{kind}, {rs(name)}),",
                "        vec![",
            ]
        )
        for signature in signatures:
            lines.append("            signature(")
            lines.append("                vec![")
            for arg in signature.args:
                name_expr = "None" if arg.name is None else f"Some({rs(arg.name)})"
                lines.append(
                    f"                    arg({arg.ty}, {name_expr}, {rb(arg.has_default)}),"
                )
            lines.append("                ],")
            lines.append("            ),")
        lines.extend(["        ],", "    );"])
    lines.extend(
        [
            "    SupportedFuncs::from_impls(impls)",
            "}",
            "",
            "fn supported_call(kind: CallKind, name: &str) -> SupportedCall {",
            "    SupportedCall { kind, name: name.to_string() }",
            "}",
            "",
            "fn signature(",
            "    args: Vec<SupportedArg>,",
            ") -> SupportedSignature {",
            "    SupportedSignature { args: args.into_boxed_slice() }",
            "}",
            "",
            "fn arg(ty: SupportedTy, argument_name: Option<&str>, has_default: bool) -> SupportedArg {",
            "    SupportedArg { ty, argument_name: argument_name.map(str::to_string), has_default }",
            "}",
            "",
        ]
    )
    return "\n".join(lines)


def rb(value: bool) -> str:
    return "true" if value else "false"


def rs(value: str) -> str:
    out = ['"']
    for ch in value:
        codepoint = ord(ch)
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\r":
            out.append("\\r")
        elif ch == "\t":
            out.append("\\t")
        elif codepoint < 0x20 or codepoint > 0x7E:
            out.append(f"\\u{{{codepoint:x}}}")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def main():
    parser = argparse.ArgumentParser(
        description="Generate accel's supported-function Rust data from SUPPORTED_FUNCS.data."
    )
    parser.add_argument("supported_funcs", type=Path)
    parser.add_argument("-o", "--output", type=Path)
    args = parser.parse_args()

    generated = render(
        parse_supported_funcs(args.supported_funcs.read_text()), args.supported_funcs
    )
    if args.output is None:
        print(generated, end="")
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(generated)


if __name__ == "__main__":
    main()
