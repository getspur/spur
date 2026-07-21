# Notebook Disk Durability & Multi-Instance Consistency

**Status:** approved  
**Canonical spec (implementation repo):**  
`getspur/spur-notebook` → `docs/superpowers/specs/2026-07-21-notebook-disk-durability-multi-instance-design.md`

This monorepo pointer exists so SPUR agents discover the approved contract. Implementation work lands in **spur-notebook**, not this workspace (notebook source was split out).

## One-line contract

Disk is shared durable truth; agent cell mutations succeed only after a generation-aware durable commit; multi-instance uses exclusive writer + content-hash token; new process always loads from file.

## Related monorepo docs

- `docs/rca/2026-05-25-spur-notebook-mcp-direct-rust.md`  
- `docs/superpowers/specs/2026-06-10-notebook-edit-cell.md`
