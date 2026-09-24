# Quickstart

Every step below runs offline with the fake Jev and the fake provider (`cargo run -p dagos -- <command>`).

## Empty Project
Initialize DAGOS, run a message, then inspect the conversation node and ordered run events.

```bash
dagos init
dagos run "Where should durable state live?"
dagos inspect run latest     # message node, context, Jev output, IR, response, emissions, events
dagos inspect events
```

## Context
A second run carries the previous run's context, and fake Jev classifies every candidate: the first
run's message and observation become active. Nodes Jev classifies inactive leave the context but stay
in the DAG.

```bash
dagos run "Add restart tests"
dagos inspect context        # members, their order, and why each is active (carried or jev)
dagos inspect run            # carried, classification, context_added, context_removed
dagos inspect dag            # every durable node and edge
```

## IR
Set a system prompt; the next run's IR carries it, and the IR is exactly what the provider received.

```bash
dagos config --system-prompt "Answer in one sentence."
dagos run "Summarize the decisions"
dagos inspect ir
```

## Failure
Select a fake model that returns malformed JSON. The run fails with an explicit error event, the raw
output stays inspectable, and no emissions enter the DAG.

```bash
dagos run "Try this" --model fake-malformed
dagos inspect run            # failure.error_code = response_invalid, rejected reason, raw output
```

Every other failure mode is one flag away: `fake-invalid-schema`, `fake-dangling-edge`, `fake-cycle`,
`fake-timeout` (with `--inference-timeout 2`), and `fake-error`.

## The App
`dagos serve` opens the app on http://127.0.0.1:7420/: projects and chats, streamed replies with
tool calls and reviews in order, the run inspector (Jev, IR, response, events), and settings for
providers, keys, the Jev, and tools. The same state is on loopback JSON: `GET /api/overview`,
`GET /api/runs/{id|latest}`, `GET /api/conversations/{id}`, `GET /api/tools`, `GET /api/health`.

## A Real Model and Jev (network)
Save a key (read from standard input, never echoed) and choose a model; choose TypeSafe's Jev in
the app (**Jev -> Model**, `~typesafe/jev-latest`) to turn on tool exposure, recall, compaction, the
review loop, and the guard. Without it, the offline classifier keeps every run working.

```bash
dagos keys set openrouter
dagos config --provider openrouter --model openai/gpt-5
dagos run --new "Where should durable state live?"
```

## Tools
Add an MCP server in the app (**Settings -> Tools**) or in `.dagos/mcp.json`, and set each tool's
policy. `ask` calls wait for Allow / Deny in the chat; `dagos run` can only use `allow` tools. A
risky call is sent to you even when its tool is `allow`, and the approval says why.

## Lint
Judge functions against the project's plain-English rules (`.dagos/lint.json`, 14 defaults); the
exit code is 1 when a rule applies, for CI. In the app, runs that change code are reviewed the same
way before they finish.

```bash
dagos lint                    # files changed since the last commit
dagos lint src/lib.rs --json  # every judgment of every function
```

## First Milestone
A complete fake-provider run works without network access; `cargo test --workspace` proves it end to
end, including tools, recall, compaction, and the review loop with scripted Jevs and servers.
