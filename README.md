# NextHome

Plan a relocation with your own AI. NextHome is a spatial database behind an
MCP server: your AI adds the places your life needs, the listings it finds,
and asks how far each one is from everything that matters.

See PLAN.md for the design and the ordered task list. Licensed AGPL-3.0.

## Developing

```sh
export DATABASE_URL='sqlite://nexthome.db?mode=rwc'
cargo run
```
