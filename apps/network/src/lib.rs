//! Network capability smoke test — Stone 14.1.
//!
//! Two buttons, one status line. The HTTPS button fires an
//! `httpbin.org/get` request and shows the returned JSON's `origin`
//! field. The WS button opens `wss://echo.websocket.events`, sends a
//! `ping`, and shows the echoed string.
//!
//! Both servers expose CORS headers and have been chosen for stability
//! over feature richness — if either disappears, replace with another
//! public echo and bump nothing else; the SDK surface is the
//! contract.

use akiva_sdk::net::{self, NetError, StreamEvent, WsEvent, WtDatagram};
use akiva_sdk::notification::{self, NotifyError, PermissionStatus};
use akiva_sdk::prelude::*;

#[derive(Default)]
struct NetworkMain {
    ui: UiState,
    /// Last status line — updated by the spawned futures via the
    /// shared `Rc<RefCell<…>>`. `&mut self` access from `on_render` is
    /// not enough because the network futures live past the frame
    /// they were spawned in.
    status: std::rc::Rc<core::cell::RefCell<String>>,
    /// Latches so a second tap while the previous round-trip is still
    /// in flight is a no-op; otherwise the user can spam open dozens
    /// of pending fetches with the run-until-idle executor.
    http_busy: std::rc::Rc<core::cell::Cell<bool>>,
    ws_busy: std::rc::Rc<core::cell::Cell<bool>>,
    wt_busy: std::rc::Rc<core::cell::Cell<bool>>,
    notify_busy: std::rc::Rc<core::cell::Cell<bool>>,
}

#[async_trait(?Send)]
impl Activity for NetworkMain {
    async fn on_create(&mut self, _ctx: &mut Context<'_>) {
        *self.status.borrow_mut() = "ready — tap a button to test".into();
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff10141c);
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("Network").size(28.0).color(0xfff0f4fa).build();
        let subtitle = ui.text("Stone 14.1 — net capability")
            .size(13.0).color(0xff8090a8).build();

        let http_btn = ui.button("http", "HTTPS GET");
        let ws_btn = ui.button("ws", "WebSocket echo");
        let wt_btn = ui.button("wt", "WebTransport echo");
        let notify_btn = ui.button("notify", "Send notification");
        let buttons = ui
            .h_stack(&[http_btn, ws_btn, wt_btn, notify_btn])
            .gap(12.0)
            .build();

        let status_text = self.status.borrow().clone();
        let status = ui.text(&status_text).size(14.0).color(0xffd0d8e8).build();

        let root = ui.v_stack(&[title, subtitle, buttons, status])
            .gap(16.0)
            .padding(24.0)
            .build();

        ui.present(root);

        // ── HTTPS GET ──────────────────────────────────────────────
        if ui.was_clicked("http") && !self.http_busy.get() {
            self.http_busy.set(true);
            *self.status.borrow_mut() = "GET httpbin.org/get …".into();
            let status = self.status.clone();
            let busy = self.http_busy.clone();
            ctx.activity_scope().spawn(async move {
                let summary = match net::get("https://httpbin.org/get").await {
                    Ok(resp) => {
                        format!(
                            "HTTPS {} ({} bytes, {} headers)",
                            resp.status,
                            resp.body.len(),
                            resp.headers.len(),
                        )
                    }
                    Err(NetError::Denied) => "denied — no NETWORK grant".into(),
                    Err(NetError::Network(reason)) => format!("network error: {reason}"),
                    Err(other) => format!("error: {other:?}"),
                };
                *status.borrow_mut() = summary;
                busy.set(false);
            });
        }

        // ── WebSocket echo ─────────────────────────────────────────
        if ui.was_clicked("ws") && !self.ws_busy.get() {
            self.ws_busy.set(true);
            // Try a list of public echo endpoints in order — DNS or
            // operator outages on any single one shouldn't break the
            // demo. Postman first because it has been more stable in
            // 2026 than the historic `echo.websocket.events`.
            const ECHO_URLS: &[&str] = &[
                "wss://ws.postman-echo.com/raw",
                "wss://echo.websocket.events",
            ];
            *self.status.borrow_mut() = format!("connecting {} …", ECHO_URLS[0]);
            let status = self.status.clone();
            let busy = self.ws_busy.clone();
            ctx.activity_scope().spawn(async move {
                let mut ws_opt = None;
                let mut last_err = None;
                for url in ECHO_URLS {
                    *status.borrow_mut() = format!("connecting {url} …");
                    match net::WebSocket::connect(*url).await {
                        Ok(w) => { ws_opt = Some(w); break; }
                        Err(e) => { last_err = Some((*url, e)); }
                    }
                }
                let ws = match ws_opt {
                    Some(w) => w,
                    None => {
                        if let Some((url, e)) = last_err {
                            *status.borrow_mut() = format!("all WS endpoints failed; last: {url} → {e:?}");
                        } else {
                            *status.borrow_mut() = "no WS endpoints configured".into();
                        }
                        busy.set(false);
                        return;
                    }
                };
                ws.send_text("ping from akiva");
                // Some servers greet first, then echo our message.
                // Loop until we see our own payload bounce back so the
                // demo has a clear "round-trip succeeded" signal.
                let mut got_echo = false;
                for _ in 0..4 {
                    match ws.recv().await {
                        Ok(WsEvent::Text(s)) => {
                            *status.borrow_mut() = format!("ws ← \"{s}\"");
                            if s.contains("ping from akiva") {
                                got_echo = true;
                                break;
                            }
                        }
                        Ok(WsEvent::Binary(b)) => {
                            *status.borrow_mut() = format!("ws ← {} binary bytes", b.len());
                        }
                        Ok(WsEvent::Closed { code, reason }) => {
                            *status.borrow_mut() =
                                format!("ws closed early: {code} {reason}");
                            break;
                        }
                        Err(e) => {
                            *status.borrow_mut() = format!("ws recv error: {e:?}");
                            break;
                        }
                    }
                }
                if got_echo {
                    *status.borrow_mut() = "ws round-trip OK".into();
                }
                ws.close(1000, "done");
                busy.set(false);
            });
        }

        // ── WebTransport echo (datagram + stream) ──────────────────
        //
        // Note: stable public WebTransport echo servers are sparse —
        // unlike WebSocket, where echo.websocket.events / Postman
        // cover the demo case. The endpoint below is a best-effort
        // candidate; failure is treated as expected and the status
        // line surfaces the reason. The button still exercises the
        // full wire path (open, send_datagram, recv_datagram,
        // open_bidi_stream, send/recv on stream, close) so the
        // unit-tested SDK gets a smoke run on the live host.
        //
        // To target your own server set
        // `localStorage.setItem('akiva.wt_endpoint', 'https://...')`
        // in the host page. The override is read by the kernel
        // worker on connect.
        if ui.was_clicked("wt") && !self.wt_busy.get() {
            self.wt_busy.set(true);
            let url = "https://webtransport.day/echo";
            *self.status.borrow_mut() = format!("connecting {url} …");
            let status = self.status.clone();
            let busy = self.wt_busy.clone();
            ctx.activity_scope().spawn(async move {
                let wt = match net::WebTransport::connect(url).await {
                    Ok(w) => w,
                    Err(e) => {
                        *status.borrow_mut() = format!("wt connect failed: {e:?}");
                        busy.set(false);
                        return;
                    }
                };
                wt.send_datagram(b"ping-dg");
                let dg_status = match wt.recv_datagram().await {
                    Ok(WtDatagram::Bytes(b)) => format!("dg ← {} bytes", b.len()),
                    Ok(WtDatagram::Closed { code, .. }) => format!("dg closed: {code}"),
                    Err(e) => format!("dg err: {e:?}"),
                };
                *status.borrow_mut() = format!("wt {dg_status}; opening stream …");
                let stream = match wt.open_bidi_stream().await {
                    Ok(s) => s,
                    Err(e) => {
                        *status.borrow_mut() = format!("wt {dg_status}; stream open failed: {e:?}");
                        wt.close(0, "");
                        busy.set(false);
                        return;
                    }
                };
                stream.send(b"ping-stream");
                let stream_status = match stream.recv().await {
                    Ok(StreamEvent::Bytes(b)) => format!("stream ← {} bytes", b.len()),
                    Ok(StreamEvent::Fin) => "stream closed by peer".into(),
                    Ok(StreamEvent::Error(s)) => format!("stream err: {s}"),
                    Err(e) => format!("stream recv err: {e:?}"),
                };
                *status.borrow_mut() = format!("wt {dg_status}; {stream_status}");
                stream.close(0);
                wt.close(0, "done");
                busy.set(false);
            });
        }

        // ── System notification ────────────────────────────────────
        //
        // Stone 14.3. The browser exposes `Notification.permission`
        // separately from the manifest grant — apps must request it
        // from a user-gesture path (this click handler counts) before
        // posts succeed. We surface every step in the status line so
        // the demo doubles as a permission-flow walkthrough.
        if ui.was_clicked("notify") && !self.notify_busy.get() {
            self.notify_busy.set(true);
            *self.status.borrow_mut() = "checking notification permission …".into();
            let status = self.status.clone();
            let busy = self.notify_busy.clone();
            ctx.activity_scope().spawn(async move {
                let perm = match notification::request_permission().await {
                    Ok(p) => p,
                    Err(e) => {
                        *status.borrow_mut() = format!("notify perm error: {e:?}");
                        busy.set(false);
                        return;
                    }
                };
                if !perm.is_granted() {
                    *status.borrow_mut() = match perm {
                        PermissionStatus::Denied => "notification permission denied".into(),
                        PermissionStatus::Default => "permission prompt dismissed".into(),
                        PermissionStatus::Granted => unreachable!(),
                    };
                    busy.set(false);
                    return;
                }
                match notification::post("Akiva", "Hello from the Network demo").await {
                    Ok(handle) => {
                        *status.borrow_mut() = format!("posted notification #{}", handle.id());
                        // Drop the handle here; `Drop` will close the
                        // notification, which is cosmetic (the OS
                        // notification center keeps the entry until the
                        // user dismisses it). To leave the notification
                        // up indefinitely, store the handle in the
                        // activity instead.
                    }
                    Err(NotifyError::PermissionNotGranted) => {
                        *status.borrow_mut() = "post failed: browser denied permission".into();
                    }
                    Err(e) => {
                        *status.borrow_mut() = format!("post failed: {e:?}");
                    }
                }
                busy.set(false);
            });
        }
    }
}

akiva_sdk::akiva_app!(NetworkMain);
