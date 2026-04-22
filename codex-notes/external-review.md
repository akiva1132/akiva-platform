# External Review — Akiva Platform

_Date: 2026-04-22_

## Purpose

This document summarizes an external architectural review of the Akiva Platform codebase and design docs.

The goal is not to flatter the project and not to restate the official vision, but to answer a narrower question:

**Is the project on a technically viable path toward becoming a real browser-hosted application OS platform that can deliver Android-like app feel while the tab is open and active?**

---

## Executive Summary

### Bottom line

**Yes, the direction is technically viable.**

There is no obvious architectural mistake in the current skeleton that makes the target impossible.

The current system is **not** just a toy and **not** a fake emulator. It already shows real OS-style structure:

- kernel/framework/sdk/runtime layering
- app ABI boundary
- package model
- activity lifecycle and task semantics
- saved-state persistence
- shell + launcher behavior
- worker/canvas-based runtime rather than DOM-driven UI

However:

**the hardest and most decisive parts are still ahead.**

What exists today proves that the team can build a coherent platform skeleton.
What is **not** yet proven is whether the platform can survive the real systems problems:

- process/fault isolation
- resource governance
- robust task/window orchestration under load

So the honest conclusion is:

**The project is on the right path, but the path has not yet crossed the hardest systems threshold.**

---

## Core Conclusion

If the target is:

- a browser-hosted application platform
- with no DOM as the primary UI layer
- with OS-like lifecycle, packaging, and activity/task semantics
- capable of delivering native-like app feel while the tab is open and active

then:

**the project is on a valid path.**

If development continues with discipline and if the deferred systems concerns are promoted and solved well, the platform could plausibly reach the point where apps in classes like:

- launcher/shell
- calculator/games
- feed-style apps
- map-style apps

feel close to real Android apps during active foreground use.

This is not guaranteed by the current docs alone, but the current docs and code do **not** block that outcome.

---

## What Is Already Strong

### 1. Architectural decomposition

The separation between:

- `kernel/`
- `framework/`
- `sdk-rust/`
- `runtime/web/`
- `apps/`

is strong and system-oriented.

This is not the structure of a demo app. It is the structure of a runtime/platform effort.

### 2. ABI discipline

The decision to keep the host/framework boundary centered around paint ops instead of widget trees is a strong architectural choice.

It keeps:

- the runtime narrower
- app-side UI evolution easier
- framework coupling lower

This is one of the best signs in the codebase.

### 3. Lifecycle/task model

The Activity/task work is real and meaningful, not decorative:

- lifecycle states
- launch modes
- back behavior
- task switching
- result delivery
- saved-state restore

These are exactly the kinds of contracts that make an app platform feel like an OS rather than a renderer.

### 4. Documentation quality

The design docs are unusually coherent with the implementation.

They do not pretend the platform is finished.
They usually distinguish between:

- implemented
- stubbed
- deferred

That is a strong positive sign.

### 5. Testing posture

The project already has meaningful test coverage across Rust and some JS runtime behavior.

That matters because platform work becomes unmanageable very quickly without invariants and regression tests.

---

## What Is Not Yet Proven

These are the areas that still separate the platform from a true OS-like core.

### 1. Process / Fault Isolation

This is the biggest unresolved systems concern.

The docs explicitly acknowledge that apps currently share too much runtime surface:

- same worker event loop
- same JS heap
- same frame budget
- same host runtime

Implication:

- one bad app can still degrade or freeze the whole system

This is recognized in the docs, but the real solution is not yet closed as design.

### 2. Resource Governance

The docs talk about:

- quotas
- memory manager
- process states
- low-memory killer
- cached/background/empty processes

But the platform does not yet have a fully specified, fully implemented governance model for:

- memory accounting per app
- eviction policy
- watchdog policy
- frame budget enforcement
- app suspension and rehydration

This is not a missing idea.
It is a missing fully-closed systems design.

### 3. Window / Task Control at the full system level

Task lifecycle control is fairly advanced.

But a true WindowManager/service-level orchestration layer is still behind the design docs.
The conceptual WindowManager is good on paper, but the concrete implementation is still intentionally stubbed.

This is important because:

- focus routing
- z-order
- display placement
- system overlays/dialogs
- transitions

are where a platform starts behaving like a real system instead of a single-app runtime.

---

## Are The Missing Parts Planned Or Missing Entirely?

### Short answer

**Mostly planned. Rarely absent.**

The review did **not** find a major foundational subsystem that is completely missing from thought and documentation.

Instead, the missing pieces fall into three buckets:

### Bucket A — planned and fairly mature

- activity/task/lifecycle control
- package/install/update model
- permissions model direction
- shell role
- display model

### Bucket B — clearly recognized, but still deferred / not fully closed

- process-level isolation
- crash recovery policy
- memory/resource governance
- text/input architecture for richer app classes

### Bucket C — implemented only partially even though the design exists

- full WindowManager behavior
- stronger resource accounting
- hard enforcement behavior around failures

So the answer is:

**the project mostly has deferred-but-recognized gaps, not invisible blind spots.**

That is much healthier than discovering that a core subsystem was never even conceptualized.

---

## Is The Current Codebase “Serious Enough”?

### Short answer

**Yes, serious enough to justify continuing.**

But not yet serious enough to declare victory.

The code written so far suggests that the authors:

- understand system boundaries
- think in terms of runtime contracts
- avoid fake shortcuts in many important places
- can translate design language into actual code structure

That is enough to take the effort seriously.

But the code written so far does **not yet prove** that the team has solved the hardest runtime-management problems.

So the current stage should be read as:

**credible skeleton, not completed platform core**

---

## Recommendation On Next Phase

### The project should move into the hard core now

Further investment in:

- extra demo apps
- surface polish
- more secondary widgets
- non-essential CLI refinement

is lower value than confronting the core systems problems.

### The next real OS-core epics should be:

1. **Process/Fault Isolation**
   Define and implement the next-generation app execution model:
   - same worker with hard containment?
   - worker per app?
   - hybrid model?
   - trap/quarantine/restart semantics?

2. **Resource Governance**
   Define and implement:
   - per-app memory accounting
   - quotas
   - low-memory killer policy
   - watchdog policy
   - warm/cold/cached app policy

3. **Window / Task System Completion**
   Move WindowManager from design/stub status into real system behavior:
   - display placement
   - overlay layers
   - focus routing
   - transition ownership
   - cross-task and cross-display determinism

These are the pieces that would turn the platform from:

- “a well-structured app runtime”

into:

- “the basis of a real application OS”

---

## Final Judgment

### What this project is

This project is:

- not a toy
- not merely a visual demo
- not a fake Android skin over the web

It is a real platform attempt with meaningful systems structure already in place.

### What it is not yet

It is not yet proven as:

- a fault-tolerant application OS core
- a resource-governing runtime
- a production-grade multi-app execution environment

### Most honest single sentence

**Akiva already looks like a credible OS-platform skeleton, but it still needs to pass the three hard systems tests — isolation, governance, and window/task control — before it can honestly claim to be a real application OS core.**

