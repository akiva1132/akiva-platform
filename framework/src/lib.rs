//! Akiva framework.
//!
//! Analogous to the AOSP system_server: sits on top of the kernel and
//! implements all the user-visible services — activity lifecycle, package
//! installation, window stacking, resource resolution, intent routing,
//! permission enforcement, AKML layout, widget tree, compositor.
//!
//! Every subsystem is its own module with a clearly-scoped API. The kernel
//! is the only legitimate caller into the framework at the top level;
//! subsystems talk to each other through typed module APIs, never via
//! globals.

pub mod activity_manager;
pub mod akml;
pub mod compositor;
pub mod intent;
pub mod layout;
pub mod package_manager;
pub mod permissions;
pub mod resource_manager;
pub mod widgets;
pub mod window_manager;

/// Framework boot. Called by the kernel during `akiva_boot`, after the
/// kernel has wired up its own singletons and the display surface is live.
pub fn boot() -> Framework {
    Framework {
        activities: activity_manager::ActivityManager::new(),
        packages: package_manager::PackageManager::new(),
        windows: window_manager::WindowManager::new(),
        resources: resource_manager::ResourceManager::new(),
        intents: intent::IntentRouter::new(),
        permissions: permissions::PermissionController::new(),
        compositor: compositor::Compositor::new(),
    }
}

/// Aggregate handle exposed to the kernel. All cross-subsystem calls go
/// through `&mut Framework` so ownership stays linear and no subsystem
/// reaches around the back.
pub struct Framework {
    pub activities: activity_manager::ActivityManager,
    pub packages: package_manager::PackageManager,
    pub windows: window_manager::WindowManager,
    pub resources: resource_manager::ResourceManager,
    pub intents: intent::IntentRouter,
    pub permissions: permissions::PermissionController,
    pub compositor: compositor::Compositor,
}
