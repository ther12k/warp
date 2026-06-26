//! One-shot TUI prompt streaming to stdout.

use std::any::Any;

use anyhow::anyhow;
use pathfinder_geometry::vector::Vector2F;
use warpui::elements::Empty;
use warpui::platform::{TerminationMode, WindowStyle};
use warpui::{
    AddWindowOptions, AppContext, Element, Entity, EntityId, ModelHandle, SingletonEntity,
    TypedActionView, View, ViewContext, ViewHandle,
};

use super::args::TuiArgs;
use super::conversation_model::{TuiConversationModel, TuiConversationModelEvent};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::AIAgentTextSection;
use crate::ai::blocklist::{
    BlocklistAIActionModel, BlocklistAIContextModel, BlocklistAIController,
    BlocklistAIHistoryModel, BlocklistAIInputModel, ConversationStatusUpdate,
    ConversationSurfaceModel,
};
use crate::ai::get_relevant_files::controller::GetRelevantFilesController;
use crate::banner::BannerState;
use crate::terminal::event::AfterBlockCompletedEvent;
use crate::terminal::local_tty::{
    TerminalManager as LocalTtyTerminalManager, TerminalSurfaceInit, TerminalSurfaceResult,
};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model::terminal_model::BlockIndex;
use crate::terminal::shared_session::IsSharedSessionCreator;
use crate::terminal::{
    PtyIntent, PtyIntentEvent, ShellLaunchData, TerminalManager as TerminalManagerTrait,
    TerminalModel, TerminalSurface,
};

struct PromptStreamHostView;

impl Entity for PromptStreamHostView {
    type Event = ();
}

impl View for PromptStreamHostView {
    fn ui_name() -> &'static str {
        "PromptStreamHostView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for PromptStreamHostView {
    type Action = ();
}

struct PromptStreamSurface {
    conversation_model: ModelHandle<TuiConversationModel>,
    last_output: String,
}

impl PromptStreamSurface {
    /// Builds the conversation-capable surface from manager-provided terminal session handles.
    fn new(surface_init: TerminalSurfaceInit, ctx: &mut ViewContext<Self>) -> Self {
        let TerminalSurfaceInit {
            model,
            sessions,
            model_events,
            ..
        } = surface_init;
        let terminal_surface_id: EntityId = ctx.view_id();
        let active_session =
            ctx.add_model(|ctx| ActiveSession::new(sessions.clone(), model_events.clone(), ctx));
        let conversation_surface = ctx.add_model(|ctx| {
            ConversationSurfaceModel::new_for_tui_surface(terminal_surface_id, ctx)
        });
        let context_model = ctx.add_model(|ctx| {
            BlocklistAIContextModel::new(
                sessions,
                &model_events,
                model.clone(),
                terminal_surface_id,
                conversation_surface.clone(),
                ctx,
            )
        });
        let input_model = ctx.add_model(|ctx| {
            BlocklistAIInputModel::new(
                model.clone(),
                conversation_surface.clone(),
                context_model.clone(),
                terminal_surface_id,
                ctx,
            )
        });
        let get_relevant_files_controller = ctx.add_model(GetRelevantFilesController::new);
        let action_model = ctx.add_model(|ctx| {
            BlocklistAIActionModel::new(
                model.clone(),
                active_session.clone(),
                &model_events,
                get_relevant_files_controller,
                terminal_surface_id,
                ctx,
            )
        });
        let ai_controller = ctx.add_model(|ctx| {
            BlocklistAIController::new(
                input_model,
                context_model.clone(),
                conversation_surface.clone(),
                action_model,
                active_session,
                model,
                terminal_surface_id,
                ctx,
            )
        });
        let conversation_model = ctx.add_model(|ctx| {
            TuiConversationModel::new(
                terminal_surface_id,
                conversation_surface,
                ai_controller,
                ctx,
            )
        });
        ctx.subscribe_to_model(&conversation_model, |surface, _, event, ctx| {
            surface.handle_conversation_event(event, ctx)
        });
        Self {
            conversation_model,
            last_output: String::new(),
        }
    }

    /// Submits a new prompt or restores and follows up in an existing conversation.
    fn submit_prompt(
        &mut self,
        prompt: String,
        conversation_id: Option<AIConversationId>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.conversation_model.update(ctx, |model, ctx| {
            if let Some(conversation_id) = conversation_id {
                model.restore_conversation_and_send_prompt(prompt, conversation_id, ctx);
            } else {
                model.send_prompt(prompt, ctx);
            }
        });
    }

    /// Adapts production model events to the one-shot stdout output.
    fn handle_conversation_event(
        &mut self,
        event: &TuiConversationModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            TuiConversationModelEvent::SelectedConversationChanged { conversation_id } => {
                let _ = conversation_id;
                self.last_output.clear();
            }
            TuiConversationModelEvent::ConversationStarted { conversation_id } => {
                println!("conversation_id={conversation_id}");
            }
            TuiConversationModelEvent::ConversationUpdated { conversation_id } => {
                self.print_stream_snapshot(*conversation_id, ctx);
            }
            TuiConversationModelEvent::ConversationStatusChanged {
                conversation_id,
                status,
                update: ConversationStatusUpdate::Changed { .. },
            } => {
                self.print_stream_snapshot(*conversation_id, ctx);
                if !status.is_in_progress() {
                    println!("status={status:?}");
                    ctx.terminate_app(TerminationMode::ForceTerminate, None);
                }
            }
            TuiConversationModelEvent::ConversationStatusChanged {
                update: ConversationStatusUpdate::Restored,
                ..
            } => {}
            TuiConversationModelEvent::Error { message } => {
                self.terminate_with_error(anyhow!("{message}"), ctx);
            }
        }
    }

    /// Prints the latest plain-text output when it changes.
    fn print_stream_snapshot(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some((has_actions, text)) = (|| {
            let exchange = BlocklistAIHistoryModel::as_ref(ctx)
                .conversation(&conversation_id)
                .and_then(|conversation| conversation.latest_exchange())?;
            let output = exchange.output_status.output()?;
            let output = output.get();
            let has_actions = output.actions().next().is_some();
            let text = output
                .text_from_agent_output()
                .flat_map(|text| text.sections.iter())
                .filter_map(|section| match section {
                    AIAgentTextSection::PlainText { text } => Some(text.text()),
                    AIAgentTextSection::Code { .. }
                    | AIAgentTextSection::Table { .. }
                    | AIAgentTextSection::Image { .. }
                    | AIAgentTextSection::MermaidDiagram { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some((has_actions, text))
        })() else {
            return;
        };
        if has_actions {
            self.terminate_with_error(
                anyhow!("TUI prompt streaming does not support tool actions"),
                ctx,
            );
            return;
        }
        if text != self.last_output {
            println!("{text}");
            self.last_output = text;
        }
    }

    /// Terminates prompt streaming with a user-visible error.
    fn terminate_with_error(&self, error: anyhow::Error, ctx: &mut ViewContext<Self>) {
        ctx.terminate_app(TerminationMode::ForceTerminate, Some(Err(error)));
    }
}

impl Entity for PromptStreamSurface {
    type Event = ();
}

impl PtyIntentEvent for () {
    fn pty_intent(&self) -> Option<PtyIntent> {
        None
    }
}

impl TerminalSurface for PromptStreamSurface {
    #[cfg(unix)]
    fn should_start_password_prompt_polling(&self, _command: &str, _ctx: &AppContext) -> bool {
        false
    }

    #[cfg(unix)]
    fn should_stop_password_prompt_polling(&self, _completed: &AfterBlockCompletedEvent) -> bool {
        false
    }

    fn on_shell_determined(&mut self, _ctx: &mut ViewContext<Self>) {}

    fn on_active_shell_launch_data_updated(
        &mut self,
        _shell_launch_data: Option<ShellLaunchData>,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    fn on_pty_spawn_failed(&mut self, error: anyhow::Error, _ctx: &mut ViewContext<Self>) {
        log::warn!("PTY spawn failed for the TUI prompt-streaming surface: {error:#}");
    }

    #[cfg(unix)]
    fn on_possible_password_prompt(
        &mut self,
        _block_index: Option<BlockIndex>,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    #[cfg(unix)]
    fn on_polled_block_completed(
        &mut self,
        _completed: &AfterBlockCompletedEvent,
        _ctx: &mut ViewContext<Self>,
    ) {
    }
}

impl View for PromptStreamSurface {
    fn ui_name() -> &'static str {
        "PromptStreamSurface"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for PromptStreamSurface {
    type Action = ();
}

impl TerminalManagerTrait for LocalTtyTerminalManager<PromptStreamSurface> {
    fn model(&self) -> std::sync::Arc<parking_lot::FairMutex<TerminalModel>> {
        self.model()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

struct PromptStreamSession {
    _manager: ModelHandle<Box<dyn TerminalManagerTrait>>,
    _surface: ViewHandle<PromptStreamSurface>,
}

impl Entity for PromptStreamSession {
    type Event = ();
}

impl SingletonEntity for PromptStreamSession {}
/// Starts prompt streaming when the TUI frontend received a prompt.
pub(super) fn start(args: TuiArgs, ctx: &mut AppContext) -> bool {
    let Some(prompt) = args.prompt else {
        return false;
    };
    start_prompt_stream(prompt, args.conversation_id, ctx);
    true
}

/// Builds a manager-backed terminal session and submits the prompt.
fn start_prompt_stream(
    prompt: String,
    conversation_id: Option<AIConversationId>,
    ctx: &mut AppContext,
) {
    let (window_id, _) = ctx.add_window(
        AddWindowOptions {
            window_style: WindowStyle::NotStealFocus,
            ..Default::default()
        },
        |_ctx| PromptStreamHostView,
    );
    let banner = ctx.add_model(|_| BannerState::default());
    let terminal_manager = LocalTtyTerminalManager::<PromptStreamSurface>::create_model(
        std::env::current_dir().ok(),
        std::env::vars_os().collect(),
        IsSharedSessionCreator::No,
        None,
        banner,
        Vector2F::new(120., 24.),
        None,
        None,
        ctx,
        move |surface_init, ctx| {
            let surface = ctx.add_typed_action_view(window_id, |ctx| {
                PromptStreamSurface::new(surface_init, ctx)
            });
            TerminalSurfaceResult {
                surface,
                post_wire: |_manager: &mut LocalTtyTerminalManager<PromptStreamSurface>,
                            _surface: &ViewHandle<PromptStreamSurface>,
                            _ctx: &mut AppContext| {},
            }
        },
    );
    let manager = terminal_manager.manager;
    let surface = terminal_manager.surface;
    surface.update(ctx, |surface, ctx| {
        surface.submit_prompt(prompt, conversation_id, ctx);
    });
    ctx.add_singleton_model(|_| PromptStreamSession {
        _manager: manager,
        _surface: surface,
    });
}
