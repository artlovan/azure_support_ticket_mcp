# Copilot CLI plugin for `azure-support-ticket-mcp`

This directory is a [Copilot CLI plugin](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-cli-plugins)
that registers `azure-support-ticket-mcp` as an MCP server in Copilot CLI.

End-users do not interact with the files in here directly — they install
the plugin with one command:

```bash
copilot plugin install artlovan/azure_support_ticket_mcp:plugin
```

…which clones this subdirectory into `~/.copilot/plugins/azure-support-ticket-mcp/`
and registers the launcher.

---

## Important: this plugin does not ship the binary

The plugin only contains the **launcher config** (`.mcp.json`). It tells
Copilot CLI to spawn the command `azure-support-ticket-mcp serve` when the
MCP is needed.

For that command to resolve, the user must first install the binary —
typically via the project's install script:

```bash
# macOS / Linux
curl -sSL https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/install.sh | sh

# Windows
irm https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/install.ps1 | iex
```

…or via `cargo install` from source. See the project [`README.md`](../README.md)
for the full install flow.

---

## Files in this directory

| File          | Purpose                                                      |
| ------------- | ------------------------------------------------------------ |
| `plugin.json` | Plugin manifest (name, version, author, points at `.mcp.json`) |
| `.mcp.json`   | MCP server launcher config (command + args)                  |
| `README.md`   | This file                                                    |

The plugin's `version` field in `plugin.json` is tracked alongside the
binary version in `Cargo.toml`. See [`../docs/RELEASING.md`](../docs/RELEASING.md)
for how the two stay in sync during releases.
