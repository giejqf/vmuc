# ADR 0010 — The CLI is a contract for scripts and agents

**Status:** accepted, 2026-10-14, at the user's request (an agent-friendly
CLI).

## Context

Coding agents and scripts drive vmuc as much as people do. What they met:

- Results were prose on stderr (`start`, `stop`, `suspend`), stdout empty;
  only `info`, `list` and `stat` had JSON.
- Every failure exited 1, so a caller could not tell a missing instance
  from one in the wrong state or a start that timed out without parsing
  English.
- `vmuc shell NAME -- CMD…` joins its arguments into one line for the
  remote shell, as ssh does: `python3 -c 'print(1)'` arrives as
  `python3 -c print(1)`.
- `vmuc edit` needs an editor; `vmuc start` on a running instance fails;
  the download progress rewrites its line with carriage returns into
  logs that are not terminals.

## Decision

- **Streams.** stdout carries the result, stderr progress and messages,
  in every mode.
- **`--json`** (global, or `VMUC_JSON=1`): on success stdout holds exactly
  one JSON document. A command on an instance (`create`, `start`,
  `resume`, `restart`, `stop`, `suspend`, `duplicate`, `edit`) prints the
  instance as `list` shows it (`name`, `status`, `addrs`, `cpus`,
  `memory`, `disk`, `dir`) plus fields of its own; `list` prints an
  array; `delete`, `reclaim`, `prune`, `template` and `info` print their
  own objects; `stat` prints one object, or one per line with `--watch`.
  On failure stdout is empty and the last line of stderr (progress may
  come before it) is `{"error": {"kind": KIND, "message": TEXT}}`.
- **Exit codes.** 0 success; 1 any other failure; 2 `invalid` (usage,
  configuration); 3 `not_found`; 4 `wrong_state` (running, stopped or
  suspended when the command needs otherwise); 5 `exists`; 6
  `timed_out`. `exec` and `shell` exit with the program's status, 125
  when vmuc fails before running it, 255 when ssh cannot reach the guest.
- **Idempotent where it is safe.** `start` on a running instance waits
  until it is ready, as a start does, and succeeds; `stop` on a stopped
  one and `suspend` on a suspended one succeed.
  `resume` refuses an instance that is not suspended and `delete` a
  running one without `--force`: both would hide a mistake.
- **`vmuc exec NAME [--workdir DIR] [--tty] -- PROGRAM ARGS…`** runs a
  program with its arguments exactly as given (each quoted for the
  remote shell), without a terminal unless asked, stdin passed through,
  in the guest path of the current directory like `shell`. `shell --
  CMD` stays a command line for the remote shell.
- **`vmuc edit NAME --set PATH=VALUE … [--cpus] [--memory] [--mount]`**
  changes the configuration without an editor, checked like an edit.
- **`vmuc list [NAME…]`** names the instances to show; an unknown one is
  `not_found`.
- **Progress without a terminal** is whole lines, no carriage returns.
- These names, fields and codes are a contract: a change may add fields
  and commands; renaming or removing one needs an ADR.

## Consequences

- An agent can drive vmuc from exit codes and JSON alone, and run
  commands in a guest without thinking about remote quoting.
- Human output is unchanged apart from `start` succeeding on a running
  instance. A VMM is this binary, so a start that fails in the VMM (a
  resume refused because the machine changed) carries the VMM's kind.
- `info --json` and `stat --json` keep working: `--json` is global.
