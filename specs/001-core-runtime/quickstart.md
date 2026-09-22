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

## Inspector API
`dagos serve` exposes the same views on loopback: `GET /api/overview`, `GET /api/runs/{id|latest}`,
`GET /api/health`.

## First Milestone
A complete fake-provider run works without network access; `cargo test --workspace` proves it end to
end.
