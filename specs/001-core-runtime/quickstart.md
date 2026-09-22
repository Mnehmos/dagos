# Quickstart

## Empty Project
Initialize DAGOS, start a fake-provider run, submit a message, then inspect the conversation node and ordered run events.

## Context
Create durable nodes, classify them with fake Jev, apply classifications, inspect active context, and confirm removed nodes still exist.

## IR
Set a system prompt, create active context, compile inference IR v1, and verify the provider sees only IR.

## Failure
Configure the fake provider to return malformed JSON. Confirm an error event is stored and no invalid semantic emissions enter the DAG.

## First Milestone
A complete fake-provider run must work without network access. This proves the architecture before adding real providers.