//! The dedicated server's files and settings, ported from
//! net.minecraft.server.dedicated and net.minecraft.server.players:
//! server.properties, eula.txt and the four user lists. The game's dedicated
//! server (`horizon-client --dedicated`) reads them, and `horizon-server`
//! manages them; this crate needs neither a window nor the game.
pub mod eula;
pub mod java_properties;
pub mod json;
pub mod properties;
pub mod user_lists;

/// The game's exit status when it saved its world and stopped to be started
/// again at once: a hosted world whose approved content mods changed loads
/// them only at start (docs/mods.md). `horizon-server` restarts it without
/// counting a crash. 75 is sysexits' EX_TEMPFAIL.
pub const ENGINE_RESTART: i32 = 75;
