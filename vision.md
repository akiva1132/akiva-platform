# Akiva
## A new application platform that runs on the browser as its runtime
### Principles and vision
**Version 0.2 — updated draft**

---

## 1. Executive summary

Akiva is a **new applicative operating system** that runs inside the
browser and lets developers build apps that feel truly native, work
on any device with a modern browser, and need no installation.

**Akiva's basic position.** Akiva is not trying to be a "better
website" or "more modern web." It treats the browser as a
**runtime / VM** — exactly the way the Android Runtime treats the
Linux kernel. Akiva deliberately gives up SEO, the DOM, and
backwards compatibility with the web ecosystem in exchange for a
truly native experience, built-in consistency, and a curated
ecosystem with a meaningful quality bar.

**Motivation.** Today's websites suffer from systemic stability,
performance, and design problems because the browser was designed
to display documents, not applications. Instead of wrapping the DOM
in another framework after another (jQuery → React → Svelte → …),
Akiva bypasses it entirely: direct rendering on Canvas / WebGPU,
WebAssembly as the runtime, and a complete application framework
on top.

**The concept in short.** Just as Android apps are written in
Java / Kotlin — existing languages — but bound to the Android
Framework (Manifest, Activity, Intent, Lifecycle), Akiva apps are
written in existing languages (Rust today; any wasm-targeting
language later) but bound to **Akiva's framework**: a system of
rules — manifest, lifecycle, intents, and a standard widget kit.

---

## 2. The problem Akiva is solving

### 2.1 The gap between websites and apps

Native apps on iOS and Android deliver a stable experience: 60 fps,
smooth animations, a consistent UI, no jarring layout jumps.
Websites, in contrast, consistently suffer from:

- Layout shifts — elements that move after load.
- Buttons of varying heights, inconsistent spacing.
- State loss on navigation.
- Performance that varies between browsers and devices.
- Slow load due to third-party scripts.
- Incoherence between apps — every site reinvents the basics.

### 2.2 Root cause

The browser was designed in 1990 as a document platform. HTML is a
language for describing documents, CSS is for styling documents,
the DOM is a document model. The whole web-app industry has spent
25 years pretending that documents are applications. Every
framework (jQuery, React, Vue, Svelte, Solid) is an attempt to
wrap the problem — not to solve it.

**Outcome:** patches on top of patches of legacy DOM, dragging the
entire ecosystem down.

---

## 3. The solution: Akiva as a third platform

### 3.1 The core concept

Akiva is **a complete applicative operating system** — not a
framework, not a library, not yet another website-building tool.
It operates at the same level as iOS and Android: it has its own
Framework, its own application store, its own quality standards,
and its own development culture.

The substantive move: **use the browser as a universal runtime**.
The browser is the one thing already installed on every computer,
phone, and tablet in the world. Instead of building a brand-new OS
that needs an install (the way ChromeOS demands dedicated
hardware), Akiva runs inside the existing browser — and turns it
into an application platform.

### 3.2 High-level architecture

Akiva's layering, inspired by Android:

```
Layer 5: Akiva Apps              — what developers write (Rust → wasm; later, any wasm target)
Layer 4: Akiva Framework         — Manifest, Activity, Intent, Lifecycle, Permissions
Layer 3: UI Toolkit              — standard widgets, design language, animations
Layer 2: Rendering Engine        — Canvas / WebGPU + WebAssembly (direct, no DOM)
Layer 1: Browser Abstraction     — hides Chrome / Safari / Firefox differences
Layer 0: Browser (as kernel)     — the universal runtime
```

**Important clarification.** The rendering layer (layer 2) is a
separate technical decision. We can choose to build our own, or
adopt Flutter Web, or any existing engine. What defines Akiva is
layers 3-4-5, not the painting layer itself.

### 3.3 What the Akiva Framework actually is

This is the heart of the proposal. Just as the Android Framework
defines rules every app must follow, the Akiva Framework defines:

**Manifest** — every app declares itself in a standard file: name,
version, required permissions, screens, intents, icon, metadata.

**Screens & Lifecycle** — every screen is a Screen with a defined
lifecycle: `on_create`, `on_start`, `on_pause`, `on_resume`,
`on_destroy`. The platform invokes them at the right moments and
preserves state automatically.

**Intents** — inter-app communication via structured messages. An
app that wants to open a PDF dispatches an intent; the platform
routes it to whichever app declared it can handle the type.

**Permissions** — a single permission model enforced by the
runtime. The user approves once; the platform enforces forever.

**State persistence** — developers do not own this. The platform
saves and restores transparently.

**Background work** — services are defined and policed by the
platform, with explicit rules on battery and resource budget.

**Navigation & URL mapping** — a Screen stack that lines up with
browser history, so the browser Back button does the expected
thing and deep links work.

### 3.4 Design principles

- **Built-in consistency**: every app uses the same standard
  widgets. A button is always the same height; an input always
  behaves the same way.
- **Hard sandbox**: every app runs in full isolation. No access to
  another app's files; no access to the device file system.
- **Offline-first**: every app must work without an internet
  connection. The platform manages sync.
- **Built-in state persistence**: no code, no thought.
- **Native accessibility**: every widget announces itself to the
  accessibility tree (not via DOM/ARIA, but via AOM — the
  Accessibility Object Model). An app that doesn't define
  accessibility doesn't pass review.
- **Standard review**: the Akiva store reviews every app. Apps
  that don't meet the standards aren't admitted. This enables a
  baseline of quality the open web cannot deliver.

### 3.5 Distribution model — open to develop, curated to distribute

A high-quality ecosystem doesn't form on its own. Platforms that
left distribution wide open fill quickly with website wrappers,
copy-paste apps, and permission misuse. On the other side, a
platform that closes the gate to developers becomes a walled
garden with no life.

Akiva's stance: **open to develop, curated to distribute**. This
isn't only a marketing decision — it's an architectural one,
because it shapes the PackageManager API, the permission model,
and publisher identity.

The principles:

- **Signed packages.** Every app has a publisher identity and a
  signed manifest. Unsigned packages are not installed from the
  primary registry.
- **Official registry by default.** The main install source has a
  quality + security gate. Apps that don't pass don't appear.
- **Sideload for developers only.** A dev-mode flag allows local
  install directly from a manifest without signing — and only in
  sessions explicitly marked as dev. Regular users won't install
  an unsigned package "by accident."
- **Cumulative publisher identity.** Reputation, violation
  history, trust tiers — these affect what an app may request in
  permissions, not just visibility in the store.

**Not in the MVP:** the registry itself, the review process,
publisher verification, trust tiers. They appear here so the
PackageManager API surface and the permission model are designed
upfront in a way that supports them — and don't need a rewrite the
moment a store appears.

---

## 4. What Akiva *is not* — important clarifications

### 4.1 Akiva is not "a better web"
It is not trying to fix the web. It builds something else that
runs on the same runtime. Just as an Android app does not "compete
with websites" — it is a separate platform.

### 4.2 Akiva does not require a new language
There is no "Akiva Script". Apps are written in existing,
widely-used, mature languages — Rust today, any wasm-targeting
language later. The novelty is in the **Framework** and the rules,
not in the language. Just as an Android app is written in regular
Kotlin / Java.

### 4.3 Akiva is not Flutter
Flutter is a UI toolkit and rendering engine. Akiva is an
applicative operating system. Flutter could, in theory, be Akiva's
rendering layer — but Flutter does not provide what Akiva
provides: Manifest, Intent, Lifecycle, store, permission model,
services, inter-app communication.

### 4.4 Akiva does not commit to SEO
Akiva apps are discovered through Akiva's store, not Google. Just
as Android apps don't "show up in Google" — they show up in the
Play Store. A developer who wants a marketing landing page can
build a regular website — Akiva is the app itself, not its
marketing page.

### 4.5 Akiva does not depend on the DOM
Direct rendering on Canvas / WebGPU, with WebAssembly as the
runtime. A deliberate choice: no patches on patches of legacy, no
browser-level layout differences, no CSS quirks.

---

## 5. Benefits

### 5.1 For end users
- Apps with native quality, no installation.
- A consistent experience — every app behaves the same way at the
  base.
- Sandbox-grade security.
- No tracking — the platform forbids it.
- No distracting ads — store standards.
- Clean uninstall — every byte gone immediately.
- Runs on Mac, PC, iPhone, Android, any device with a modern
  browser.

### 5.2 For developers
- Write once — runs on every platform.
- A familiar language (Rust today, more later).
- No browser fragmentation — a defined runtime environment.
- Standard widgets that handle the edge cases (RTL,
  accessibility, responsive).
- No need to build a backend (Akiva Cloud as an option).
- Distribution through the store.
- No need for Apple / Google approval — instant updates.

### 5.3 For the ecosystem
- A third platform, independent of Apple and Google.
- Open standards.
- A quality bar that raises the floor.

---

## 6. Drawbacks and risks

### 6.1 Technical challenges

- **Bundle size.** The Akiva runtime loads with every app. A few
  hundred KB to a first MB — could slow first load relative to a
  light HTML site. Less significant for apps users return to.
- **Text editing and IME.** Editing text on Canvas is a known
  challenge (Figma, Google Docs, Flutter Web have all hit it),
  especially the IME story for Asian languages and integration
  with the iOS system keyboard. Demands serious investment in the
  HAL.
- **Accessibility.** Akiva does not use the DOM, so it must build
  a parallel accessibility layer through AOM. Similar to what
  Android does with TalkBack — feasible and proven, but real work.
- **Cross-browser compatibility.** WebGPU, OffscreenCanvas, and
  other modern capabilities are not supported uniformly. A defined
  baseline + fallbacks are required.
- **URL and routing.** Akiva must sync with browser history (Back
  button, bookmarks, deep links) while preserving the Screen /
  Stack model. Android solved this with its Activity stack; Akiva
  needs an equivalent.

### 6.2 Strategic challenges

- **Chicken and egg.** Developers come only when there are users.
  Users come only when there are apps. Every new platform faces
  this.
- **Competing with the open web.** Developers will have to choose:
  build a regular app that works in any browser, or an Akiva app
  that works only in Akiva.
- **Big-Tech dependency.** If Google, Apple, or Microsoft
  restricts WebAssembly or WebGPU — Akiva is exposed.
- **Heavy upfront investment.** Building a complete platform
  takes years of development, a skilled team, and millions of
  dollars.

---

## 7. Technological feasibility

### 7.1 Overall assessment

Technologically, Akiva can be implemented in full. Every component
exists, is stable, and is supported by modern browsers. There is
no research breakthrough required — only integration and the right
combination of mature technologies.

### 7.2 Existing technologies that enable this

- **WebAssembly** — a fast runtime for code from any language
  (Rust, C++, Go).
- **WebGPU** — direct GPU access via a modern API.
- **OffscreenCanvas + Web Workers** — real multi-threading.
- **OPFS (Origin Private File System)** — sandboxed file system.
- **Service Workers + PWA APIs** — install-to-home, push,
  offline.
- **Hardware APIs** — WebUSB, Web Bluetooth, WebHID,
  MediaDevices, Geolocation.
- **WebAuthn + Payment Request API** — strong auth and native
  payments.
- **AOM (Accessibility Object Model)** — accessibility without
  DOM.

### 7.3 Projects that prove parts of the concept

Makepad (Rust), Flutter Web (Google), Figma — all prove that
wasm-first + canvas rendering works. **Akiva takes their technical
foundation and builds a complete platform** with Framework, store,
and standards above it. That part is the missing piece.

### 7.4 Effort estimate

- **Functional MVP:** 3–5 engineers, 12–18 months, $2–5 M.
- **Mature platform with ecosystem:** 15–30 people, 3–5 years,
  $10–30 M.
- **Critical mass:** 5–10 years, depending on marketing success.

---

## 8. Market feasibility

### 8.1 Overall assessment

More complex than the technological feasibility. Requires
simultaneously convincing developers and end users.

### 8.2 Possible target audiences

- **Vertical strategy:** focus initially on a single vertical
  where the value proposition is strongest — SaaS tools for SMBs,
  creative tools, or internal enterprise apps.
- **Web-frustrated developers:** developers tired of modern
  frontend who are looking for a more native-like development
  experience.
- **Emerging markets:** India, Africa, South-East Asia — already
  high PWA adoption.

### 8.3 Core messages

- **For developers:** build once, run anywhere, at native
  quality, on a platform that takes care of accessibility, RTL,
  responsive, and performance for you.
- **For users:** apps that work great, no installation, no
  tracking, on any device.
- **For businesses:** instead of building three times, build once.
  Instead of 30 % store fees, reach the user directly.

### 8.4 Market risks

- Conservative developers — demand proof of ROI.
- Apple / Google may restrict capabilities in the browser.
- A market crowded with attempts — "yet another platform."
- Trust — developers will not build a business on a platform that
  could disappear.

---

## 9. Success model

The Akiva platform will be considered successful if, within five
years:

- 10 000 active developers.
- 1 000 000 monthly active users.
- 100 flagship apps at professional quality.
- A stable revenue model — store cut, Akiva Cloud, or enterprise.
- Recognition as a legitimate third platform.

---

## 10. Basic development steps

### Phase 1: detailed planning (months 1–2)
- Full spec of the Akiva Framework — Manifest, Activity, Intent,
  Lifecycle, Permissions.
- Definition of the Standard Library of widgets.
- Definition of the permission model.
- Definition of the Akiva Design Language.
- Decision on the rendering layer (build our own vs. adopt).

### Phase 2: Proof of Concept (months 3–6)
- Minimal runtime in Rust → WebAssembly.
- Basic layout engine (Flexbox-like).
- 5–10 basic widgets.
- Event system.
- Demo apps (Notes, Counter, Todo).

### Phase 3: SDK and tooling (months 7–12)
- CLI (`akiva init`, `build`, `run`).
- Dev server with hot reload.
- Inspector / debugger.
- Initial documentation.

### Phase 4: private alpha (months 13–18)
- 20–50 alpha developers.
- 5–10 reference apps.
- Iteration on the API.
- Performance stabilisation.

### Phase 5: public beta + store (year 2)
- Open SDK.
- `akiva.store` with a review process.
- Developer portal.
- Akiva Cloud (storage, auth, push).

### Phase 6: 1.0 launch (year 3)
- Public launch.
- Stable funding model.
- Partnerships with 3–5 major companies.
- Conferences, education, content.

### Phase 7: growth (years 4–5)
- Extended capabilities (3D, AR, AI).
- Additional markets.
- More tooling (IDE, linters, testing).
- Contribution to W3C / WHATWG standards.

---

## 11. Summary

Akiva is a proposal for **a third platform** of applications — an
applicative operating system that runs on the browser as a
runtime, exactly the way Android runs on the Linux kernel.

It is not a website, not a framework, not a website-builder. It
deliberately gives up SEO, the DOM, and backwards compatibility
with the web — in exchange for a truly native experience, built-in
consistency, and a curated ecosystem with a meaningful quality
standard.

**The concept is technologically valid** — every component
exists; what's needed is integration and execution.

**The concept is market-complex** — it requires a solution to the
chicken-and-egg problem.

**The concept is unique relative to existing options** — Flutter,
React Native, PWA each solve part of the problem. None offers a
complete platform with Framework, store, review, and quality
standards.

Success requires a clear vision, consistent execution, long-term
funding, and a skilled team. But it is possible, and the platform
holds the potential to become the world's third application
platform — alongside iOS and Android.

---

*This document is a starting point. It is not a business plan or a
final technical specification.*
*Version 0.2 — April 2026*
