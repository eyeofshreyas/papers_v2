use crate::deps::*;

use std::cell::Cell;
use std::path::PathBuf;

use futures::FutureExt;
use futures::future::LocalBoxFuture;
use glib::closure_local;
use papers_document::{DocumentAnnotations, DocumentForms, DocumentMode, LinkDest};
use papers_view::{JobLoad, JobPriority, SizingMode};

mod imp {
    use super::*;

    #[derive(Properties, Default, Debug, CompositeTemplate)]
    #[properties(wrapper_type = super::PpsTab)]
    #[template(resource = "/org/gnome/papers/ui/tab.ui")]
    pub struct PpsTab {
        // Template children
        #[template_child]
        pub(super) stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub(super) loader_view: TemplateChild<PpsLoaderView>,
        #[template_child]
        pub(super) error_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(super) password_view: TemplateChild<PpsPasswordView>,
        #[template_child]
        pub(super) document_view: TemplateChild<PpsDocumentView>,
        #[template_child]
        pub(super) presentation: TemplateChild<papers_view::ViewPresentation>,

        // GObject property exposed for AdwTabPage.title binding
        #[property(get, set, explicit_notify)]
        pub(super) display_name: RefCell<String>,

        // Per-document state (moved from PpsWindow)
        pub(super) mode: Cell<WindowRunMode>,
        pub(super) edit_name: RefCell<String>,
        pub(super) metadata: RefCell<Option<papers_view::Metadata>>,
        pub(super) dest: RefCell<Option<papers_document::LinkDest>>,
        pub(super) monitor: RefCell<Option<PpsFileMonitor>>,
        pub(super) local_path: RefCell<Option<PathBuf>>,
        pub(super) file: RefCell<Option<gio::File>>,
        pub(super) uri_mtime: Cell<i64>,
        pub(super) load_job: RefCell<Option<papers_view::JobLoad>>,
        pub(super) load_job_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) reload_job: RefCell<Option<papers_view::JobLoad>>,
        pub(super) reload_job_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PpsTab {
        const NAME: &'static str = "PpsTab";
        type Type = super::PpsTab;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            PpsLoaderView::ensure_type();
            PpsPasswordView::ensure_type();
            PpsDocumentView::ensure_type();
            papers_view::ViewPresentation::ensure_type();
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for PpsTab {
        fn dispose(&self) {
            self.clear_local_uri();
            self.obj().first_child().map(|w| w.unparent());
        }
    }

    impl WidgetImpl for PpsTab {}

    #[gtk::template_callbacks]
    impl PpsTab {
        fn load_job(&self) -> Option<JobLoad> {
            self.load_job.borrow().clone()
        }

        pub(super) fn clear_local_uri(&self) {
            if let Some(path) = self.local_path.take() {
                let _ = std::fs::remove_file(path);
            }
        }

        pub(super) fn clear_load_job(&self) {
            if let Some(job) = self.load_job.take() {
                if !job.is_finished() {
                    job.cancel();
                }
                if let Some(id) = self.load_job_handler.take() {
                    job.disconnect(id);
                }
            }
        }

        pub(super) fn clear_reload_job(&self) {
            if let Some(job) = self.reload_job.take() {
                if !job.is_finished() {
                    job.cancel();
                }
                if let Some(id) = self.reload_job_handler.take() {
                    job.disconnect(id);
                }
            }
        }

        pub(super) fn set_mode(&self, mode: WindowRunMode) {
            if self.mode.get() == mode {
                return;
            }
            self.mode.set(mode);
            match mode {
                WindowRunMode::Normal => self.stack.set_visible_child_name("document"),
                WindowRunMode::PasswordView => self.stack.set_visible_child_name("password"),
                WindowRunMode::StartView => self.stack.set_visible_child_name("document"),
                WindowRunMode::LoaderView => self.stack.set_visible_child_name("loader"),
                WindowRunMode::ErrorView => self.stack.set_visible_child_name("error"),
                WindowRunMode::Presentation => self.stack.set_visible_child_name("presentation"),
                WindowRunMode::Fullscreen => self.stack.set_visible_child_name("document"),
            }
        }

        // Template callbacks

        #[template_callback]
        fn loader_view_cancelled(&self) {
            // Nothing to cancel yet — just stay on loader page.
        }

        #[template_callback]
        fn password_view_unlock(&self, password: &str, flags: gio::PasswordSave) {
            if let Some(load_job) = self.load_job() {
                load_job.set_password(Some(password));
                load_job.set_password_save(flags);
                load_job.scheduler_push_job(JobPriority::PriorityNone);
            }
        }

        #[template_callback]
        fn password_view_cancelled(&self) {
            if self.mode.get() == WindowRunMode::StartView {
                self.clear_load_job();
            }
        }

        #[template_callback]
        fn presentation_finished(&self) {
            WidgetExt::activate_action(self.obj().as_ref(), "win.escape", None)
                .expect("Can't activate action win.escape");
        }

        #[template_callback]
        fn external_link_clicked(&self, action: &papers_document::LinkAction) {
            use papers_document::LinkActionType;
            if action.action_type() == LinkActionType::ExternalUri {
                let context = WidgetExt::display(&self.obj().clone()).app_launch_context();
                let uri = action.uri().unwrap();
                let file = gio::File::for_uri(&uri);
                let uri = if file.uri_scheme().is_some() {
                    uri.to_string()
                } else if uri.starts_with("www.") {
                    format!("https://{uri}")
                } else {
                    return;
                };
                glib::spawn_future_local(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    async move {
                        if let Err(e) =
                            gio::AppInfo::launch_default_for_uri_future(&uri, Some(&context)).await
                        {
                            obj.show_error_message(
                                Some(&e),
                                &gettext("Unable to open external link"),
                            );
                        }
                    }
                ));
            }
        }

        // Error helpers — route to the parent window's toast overlay

        pub(super) fn show_error_message(&self, error: Option<&glib::Error>, msg: &str) {
            let Some(window) = self.obj().root().and_downcast::<crate::window::PpsWindow>() else {
                return;
            };
            window.show_error_message(error, msg);
        }

        pub(super) fn show_error(&self, error: Option<&glib::Error>) {
            self.error_page.set_description(error.map(|e| e.message()));
            self.set_mode(WindowRunMode::ErrorView);
        }
    }
}

glib::wrapper! {
    pub struct PpsTab(ObjectSubclass<imp::PpsTab>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PpsTab {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn uri(&self) -> Option<String> {
        self.imp().file.borrow().as_ref().map(|f| f.uri().into())
    }

    pub fn is_empty(&self) -> bool {
        self.imp().document_view.is_empty() && self.imp().load_job.borrow().is_none()
    }

    pub fn check_document_modified(&self) -> Option<String> {
        let document = self.imp().document_view.model().document()?;
        let forms_modified = document
            .dynamic_cast_ref::<DocumentForms>()
            .map(|d| d.document_is_modified())
            .unwrap_or_default();
        let annots_modified = document
            .dynamic_cast_ref::<DocumentAnnotations>()
            .map(|d| d.document_is_modified())
            .unwrap_or_default();
        match (forms_modified, annots_modified) {
            (true, true) => Some(gettext(
                "Document contains new or modified annotations and form fields that have been filled out.",
            )),
            (true, false) => Some(gettext(
                "Document contains form fields that have been filled out.",
            )),
            (false, true) => Some(gettext("Document contains new or modified annotations.")),
            (false, false) => None,
        }
    }

    pub fn close_handled(&self) -> glib::Propagation {
        self.imp().document_view.close_handled()
    }

    pub fn document_view(&self) -> &PpsDocumentView {
        &self.imp().document_view
    }
}

impl Default for PpsTab {
    fn default() -> Self {
        Self::new()
    }
}
