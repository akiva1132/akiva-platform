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

use akiva_sdk::net::{self, NetError, WsEvent};
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
        let buttons = ui.h_stack(&[http_btn, ws_btn]).gap(12.0).build();

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
    }
}

akiva_sdk::akiva_app!(NetworkMain);
