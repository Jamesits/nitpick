## Documentation
- Use ASD-STE100
- DO use only short, simple and precise wording
- DO NOT correct yourself in the doc; DO remove the wrong statements and replace with correct ones
- DO state which claims are unverified or in question in the doc
- DO highlight deviations from textbook algorithms/implementations
- DO NOT reference temporary things or artifacts outside the git repo (e.g. anything under `tmp`); directly include description, algorithms, methods or code/pseudo-code snippets
- DO NOT duplicate information; write in one place, and reference it from others
- Markdown rules: Use mermaid for graphs; use MathJax (inline or block) for formulae; escape absolute/determinant/pipe operators in tables; no ToC
- Plan stages and numbering is ephemeral, do not reference them outside a plan; summary the text instead

### Design (`doc/design`)
Write:
- Overview: the scope, the architecture, the interface/boundary (how it is connected to other modules)
- Technical decisions (Architectural only; reasoning of a specific implementation goes to code comments): considered and selected options each with a short reasoning; counter-intuitive decisions
- Usage (IFF different from the standard scaffold usage OR require manual timing): requirements/dependencies, pre/post run scripts, commands
- Testing: correctness/performance target and test method
- Future features/enhancements not in the current plan

Validate:
- Future enhancements and current plan do not interfere; otherwise STOP and ask

### Plans (`doc/plan`)
Numbered H3 sections, in dependency order, that consists of:
- Open questions about design docs and implementation
- TODO actions (what to implement/what problem to fix + how to verify if it is done)

Requirements:
- DO remove what you have done or validated
- DO NOT renumber the items when removing done ones

### Code Comment
- DO document what the code cannot say: rationale, invariants, units, edge cases, counter-intuitive behavior, measured values behind constants
- DO NOT restate what the adjacent code does; no step narration

## Code
- Write modular and reproducible code

### Rust
- DO run clippy and cargo format after each job and fix all lint issues
- DO use nextest
- DO search for existing libraries before creating one

### Tests
- DO write unit tests in a separate `<file>_tests.rs` file
