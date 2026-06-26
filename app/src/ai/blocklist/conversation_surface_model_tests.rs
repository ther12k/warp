use std::sync::Arc;

use parking_lot::FairMutex;
use warp_core::features::FeatureFlag;
use warpui::r#async::executor::Background;
use warpui::{App, EntityId, ModelHandle};

use super::ConversationSurfaceModel;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::blocklist::agent_view::{
    AgentViewController, AgentViewEntryOrigin, EphemeralMessageModel,
};
use crate::ai::blocklist::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::terminal::color::{self, Colors};
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::model::test_utils::block_size;
use crate::terminal::TerminalModel;
use crate::test_util::settings::initialize_settings_for_tests;

fn build_tui_surface(
    app: &mut App,
) -> (
    ModelHandle<BlocklistAIHistoryModel>,
    ModelHandle<ConversationSurfaceModel>,
    EntityId,
) {
    initialize_settings_for_tests(app);
    let history = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
    let terminal_surface_id = EntityId::new();
    let surface = app.add_model(|ctx| {
        ConversationSurfaceModel::new_for_tui_surface_with_history_test(terminal_surface_id, ctx)
    });
    (history, surface, terminal_surface_id)
}

#[test]
fn gui_surface_delegates_selection_to_agent_view() {
    App::test((), |mut app| async move {
        let _agent_view = FeatureFlag::AgentView.override_enabled(true);
        initialize_settings_for_tests(&mut app);
        let history = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let terminal_surface_id = EntityId::new();
        let terminal_model = Arc::new(FairMutex::new(TerminalModel::new_for_test(
            block_size(),
            color::List::from(&Colors::default()),
            ChannelEventListener::new_for_test(),
            Arc::new(Background::default()),
            false,
            None,
            false,
            false,
            None,
        )));
        let ephemeral_message_model = app.add_model(|_| EphemeralMessageModel::new());
        let agent_view_controller = app.add_model(|_| {
            AgentViewController::new(
                terminal_model,
                terminal_surface_id,
                ephemeral_message_model,
            )
        });
        let surface = app.add_model(|ctx| {
            ConversationSurfaceModel::new_for_terminal_view(
                terminal_surface_id,
                agent_view_controller.clone(),
                ctx,
            )
        });
        let conversation_id = history.update(&mut app, |history, ctx| {
            history.start_new_conversation(terminal_surface_id, false, false, false, ctx)
        });

        surface.update(&mut app, |surface, ctx| {
            surface.select_existing_conversation(
                conversation_id,
                AgentViewEntryOrigin::ConversationSelector,
                ctx,
            );
        });

        surface.read(&app, |surface, ctx| {
            assert_eq!(
                surface.selected_conversation_id(ctx),
                Some(conversation_id)
            );
        });
        agent_view_controller.read(&app, |controller, _| {
            assert!(controller.is_active());
        });
    });
}

#[test]
fn tui_surface_owns_next_prompt_selection() {
    App::test((), |mut app| async move {
        let (_, surface, _) = build_tui_surface(&mut app);
        let conversation_id = AIConversationId::new();

        surface.update(&mut app, |surface, ctx| {
            surface.select_existing_conversation(conversation_id, AgentViewEntryOrigin::Cli, ctx);
        });
        surface.read(&app, |surface, ctx| {
            assert_eq!(
                surface.selected_conversation_id(ctx),
                Some(conversation_id)
            );
        });

        surface.update(&mut app, |surface, ctx| {
            surface.select_new_conversation(AgentViewEntryOrigin::Cli, ctx);
        });
        surface.read(&app, |surface, ctx| {
            assert_eq!(surface.selected_conversation_id(ctx), None);
        });
    });
}

#[test]
fn tui_surface_creates_and_selects_terminal_surface_scoped_conversation() {
    App::test((), |mut app| async move {
        let (history, surface, terminal_surface_id) = build_tui_surface(&mut app);

        let conversation_id = surface
            .update(&mut app, |surface, ctx| {
                surface.try_start_new_conversation(AgentViewEntryOrigin::Cli, ctx)
            })
            .expect("TUI conversation creation should succeed");

        surface.read(&app, |surface, ctx| {
            assert_eq!(
                surface.selected_conversation_id(ctx),
                Some(conversation_id)
            );
        });
        history.read(&app, |history, _| {
            assert_eq!(
                history
                    .all_live_conversations_for_terminal_surface(terminal_surface_id)
                    .map(|conversation| conversation.id())
                    .collect::<Vec<_>>(),
                vec![conversation_id]
            );
        });
    });
}

#[test]
fn tui_surface_reconciles_split_and_removed_selection() {
    App::test((), |mut app| async move {
        let (history, surface, terminal_surface_id) = build_tui_surface(&mut app);
        let old_conversation_id = AIConversationId::new();
        let new_conversation_id = AIConversationId::new();

        surface.update(&mut app, |surface, ctx| {
            surface.select_existing_conversation(
                old_conversation_id,
                AgentViewEntryOrigin::Cli,
                ctx,
            );
        });
        history.update(&mut app, |_, ctx| {
            ctx.emit(BlocklistAIHistoryEvent::SplitConversation {
                terminal_surface_id,
                old_conversation_id,
                new_conversation_id,
            });
        });
        surface.read(&app, |surface, ctx| {
            assert_eq!(
                surface.selected_conversation_id(ctx),
                Some(new_conversation_id)
            );
        });

        history.update(&mut app, |_, ctx| {
            ctx.emit(BlocklistAIHistoryEvent::RemoveConversation {
                terminal_surface_id,
                conversation_id: new_conversation_id,
                run_id: None,
            });
        });
        surface.read(&app, |surface, ctx| {
            assert_eq!(surface.selected_conversation_id(ctx), None);
        });
    });
}
