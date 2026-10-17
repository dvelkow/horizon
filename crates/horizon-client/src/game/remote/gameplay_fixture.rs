//! Headless regressions through the actual Game tick and packet handlers.
//! Run tools/test_gameplay_updates.py with prepared resources and a native binary.
use super::*;
use horizon_client::blocks::Fluid;
use horizon_protocol::{connection::Pipe, entity_data::serializer};
use horizon_world::level::Ticks;
use serde_json::json;
use std::path::Path;

impl Game<'_> {
    pub(in crate::game) fn gameplay_fixture(&mut self, report: &Path) -> Result<()> {
        let mut results = Vec::new();
        let mut check = |name: &str, passed: bool| {
            results.push(json!({"name": name, "passed": passed}));
        };
        self.gameplay_fixture_world()?;
        let stone = self
            .pack
            .assets
            .registry
            .default_state("minecraft:stone")
            .unwrap();
        let dirt = self
            .pack
            .assets
            .registry
            .default_state("minecraft:dirt")
            .unwrap();
        let water = self
            .pack
            .assets
            .registry
            .default_state("minecraft:water")
            .unwrap();
        let lava = self
            .pack
            .assets
            .registry
            .default_state("minecraft:lava")
            .unwrap();
        let sand = self
            .pack
            .assets
            .registry
            .default_state("minecraft:sand")
            .unwrap();
        for (x, fluid, ticks) in [(2, water, 8), (-6, lava, 35)] {
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                self.world.set(x + dx, 1, 2 + dz, stone)?;
            }
            self.world.set(x, 1, 2, fluid)?;
            self.break_block([x + 1, 1, 2])?;
            for _ in 0..ticks {
                self.world.game_time += 1;
                self.tick_blocks()?;
            }
            let actual = self.res.facts.get(self.world.block(x + 1, 1, 2)).fluid;
            check(
                if fluid == water {
                    "water_refills_broken_wall_without_host"
                } else {
                    "lava_refills_broken_wall_without_host"
                },
                actual
                    == if fluid == water {
                        Fluid::Water
                    } else {
                        Fluid::Lava
                    },
            );
        }
        self.break_block([2, 0, 2])?;
        for _ in 0..8 {
            self.world.game_time += 1;
            self.tick_blocks()?;
        }
        check(
            "water_falls_into_broken_floor",
            self.res.facts.get(self.world.block(2, 0, 2)).fluid == Fluid::Water,
        );

        self.world.set(8, 3, 8, dirt)?;
        self.world.set(8, 4, 8, sand)?;
        self.break_block([8, 3, 8])?;
        check(
            "survival_break_creates_loot",
            self.dropped
                .iter()
                .any(|d| &*d.stack.item == "minecraft:dirt"),
        );
        for _ in 0..50 {
            self.world.game_time += 1;
            self.tick_blocks()?;
            self.tick_falling()?;
            self.tick_items();
        }
        check(
            "unsupported_sand_falls_without_host",
            self.world.block(8, 1, 8) == sand,
        );
        check(
            "drops_fall_and_pickup_delay_expires",
            self.dropped
                .iter()
                .any(|d| d.pickup_delay == 0 && d.position.y < 3.0 && d.position.y >= 1.0),
        );

        // Keep the other end alive; no network service or real account needed.
        let (client, server) = Pipe::pair();
        self.connect_local(client, "PhysicsFixture")?;
        self.gameplay_fixture_world()?;
        let builtin = &self.remote.as_ref().unwrap().builtin;
        let kind = builtin
            .get("minecraft:entity_type")
            .unwrap()
            .id("minecraft:item")
            .unwrap();
        let item = builtin
            .get("minecraft:item")
            .unwrap()
            .id("minecraft:dirt")
            .unwrap();
        self.handle_packet(C::AddEntity(play::AddEntity {
            id: 420,
            uuid: 420,
            kind,
            position: [4.5, 5.0, 4.5],
            velocity: [0.1, 0.2, 0.0],
            pitch: 0,
            yaw: 0,
            head_yaw: 0,
            data: 0,
        }))?;
        // Motion can arrive before metadata: it must survive the pending stage.
        self.handle_packet(C::SetEntityMotion(play::SetEntityMotion {
            id: 420,
            velocity: [0.2, 0.3, 0.0],
        }))?;
        self.handle_packet(C::SetEntityData(play::SetEntityData {
            id: 420,
            values: vec![(
                ITEM_ENTITY_ITEM,
                serializer::ITEM_STACK,
                Value::Item(Some(Stack::new(item, 8))),
            )],
        }))?;
        check(
            "spawn_and_early_motion_velocity_preserved",
            self.dropped[0].velocity.x == 0.2 && self.dropped[0].velocity.y == 0.3,
        );
        let initial = self.dropped[0].position;
        self.tick_remote_entities(false);
        check("frozen_drops_stay_put", self.dropped[0].position == initial);
        let mut moving_ticks = 0;
        for _ in 0..10 {
            let previous = self.dropped[0].position;
            self.tick_remote_entities(true);
            moving_ticks += usize::from(self.dropped[0].position != previous);
        }
        check(
            "drops_move_every_tick_without_new_packets",
            moving_ticks == 10,
        );
        check(
            "gravity_changes_drop_velocity",
            self.dropped[0].velocity.y < 0.0,
        );
        for _ in 0..50 {
            self.tick_remote_entities(true);
        }
        check(
            "remote_drop_lands_on_floor",
            (self.dropped[0].position.y - 1.0).abs() < 0.01,
        );
        self.handle_packet(C::EntityPositionSync(play::EntityPositionSync {
            id: 420,
            path: play::PositionPath::Linear([5.0, 3.0, 5.0]),
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        }))?;
        check(
            "server_position_corrects_prediction",
            self.dropped[0].position == Vec3::new(5.0, 3.0, 5.0),
        );
        self.handle_packet(C::MoveEntityPos(play::MoveEntityPos(play::MoveEntity {
            id: 420,
            delta: Some(play::VecDelta::Linear([4096, 0, 0])),
            rotation: None,
            on_ground: false,
        })))?;
        check(
            "relative_packets_use_server_base",
            self.dropped[0].position.x == 6.0,
        );
        self.handle_packet(C::SetEntityMotion(play::SetEntityMotion {
            id: 420,
            velocity: [0.0, 0.4, 0.0],
        }))?;
        check(
            "later_motion_packets_update_drop",
            self.dropped[0].velocity.y == 0.4,
        );
        self.handle_packet(C::TakeItemEntity(play::TakeItemEntity {
            item: 420,
            player: 99,
            amount: 3,
        }))?;
        check(
            "partial_pickup_keeps_remaining_stack",
            self.dropped.first().is_some_and(|d| d.stack.count == 5),
        );
        self.handle_packet(C::TakeItemEntity(play::TakeItemEntity {
            item: 420,
            player: 99,
            amount: 5,
        }))?;
        check("full_pickup_removes_drop", self.dropped.is_empty());
        server.shutdown();
        let passed = results.iter().all(|r| r["passed"] == true);
        std::fs::write(
            report,
            serde_json::to_vec_pretty(&json!({"passed": passed, "checks": results}))?,
        )?;
        if passed {
            Ok(())
        } else {
            Err("gameplay regression failed; see report".into())
        }
    }

    fn gameplay_fixture_world(&mut self) -> Result<()> {
        self.world = World::new(self.res.facts.air, 12345);
        for x in -1..=1 {
            for z in -1..=1 {
                let mut chunk = Chunk::empty(x, z);
                chunk.lit = true;
                self.world.chunks.insert((x, z), Arc::new(chunk));
            }
        }
        let stone = self
            .pack
            .assets
            .registry
            .default_state("minecraft:stone")
            .unwrap();
        for x in -16..32 {
            for z in -16..32 {
                self.world.set(x, 0, z, stone)?;
            }
        }
        self.player = None;
        self.creative = false;
        self.streaming.extra = vec![((0, 0), 3)];
        self.settings.simulation_distance = 3;
        self.block_ticks = Ticks::new(0);
        self.tick_chunks.clear();
        self.sync_tick_chunks();
        self.level_events.clear();
        self.dropped.clear();
        self.falling.clear();
        Ok(())
    }
}
