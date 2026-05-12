use crate::deps::*;

use std::cell::Cell;
use std::path::PathBuf;

use futures::FutureExt;
use futures::future::LocalBoxFuture;
use glib::closure_local;
use papers_document::{DocumentAnnotations, DocumentForms, DocumentMode};
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
        #[property(get, set)]
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

        pub(super) fn set_filenames(&self, file: &gio::File) {
            let attributes = [
                gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME.as_str(),
                gio::FILE_ATTRIBUTE_STANDARD_EDIT_NAME.as_str(),
            ]
            .join(",");
            let info = file.query_info(
                &attributes,
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            );
            let (display_name, edit_name) = match info {
                Ok(info) => {
                    let display_name = info.display_name().to_string();
                    let edit_name = if info.has_attribute(gio::FILE_ATTRIBUTE_STANDARD_EDIT_NAME) {
                        info.edit_name().to_string()
                    } else {
                        display_name.clone()
                    };
                    (display_name, edit_name)
                }
                Err(e) => {
                    glib::g_warning!("", "Failed to query file info: {}", e.message());
                    let basename = file.basename().unwrap().display().to_string();
                    (basename.clone(), basename)
                }
            };
            self.document_view.set_filenames(&display_name, &edit_name);
            self.obj().set_display_name(display_name);
            self.edit_name.replace(edit_name);
        }

        pub(super) fn init_metadata_with_default_values(&self, file: &gio::File) {
            if !papers_view::Metadata::is_file_supported(file) {
                return;
            }
            let metadata = papers_view::Metadata::new(file);
            let default_settings = gio::Settings::new("org.gnome.Papers.Default");
            let boolean_properties = [
                "continuous",
                "dual-page",
                "dual-page-odd-left",
                "fullscreen",
                "window-maximized",
            ];
            for property in boolean_properties {
                if !metadata.has_key(property) {
                    metadata.set_boolean(property, default_settings.boolean(property));
                }
            }
            if !metadata.has_key("rtl") {
                metadata.set_boolean(
                    "rtl",
                    gtk::Widget::default_direction() == gtk::TextDirection::Rtl,
                );
            }
            if !metadata.has_key("sizing-mode") {
                let enum_class = glib::EnumClass::new::<SizingMode>();
                let mode = default_settings.enum_("sizing-mode");
                let mode = enum_class.value(mode).unwrap().nick();
                metadata.set_string("sizing-mode", mode);
            }
            if !metadata.has_key("zoom") {
                metadata.set_double("zoom", default_settings.double("zoom"));
            }
            self.metadata.replace(Some(metadata));
        }

        fn load_remote_failed(&self, error: &glib::Error) {
            self.show_error_message(
                Some(error),
                &self
                    .local_path
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .display()
                    .to_string(),
            );
            self.clear_local_uri();
            self.uri_mtime.set(0);
        }

        async fn remote_file_copy_ready(
            &self,
            source: &gio::File,
            result: Result<(), glib::Error>,
        ) {
            let Err(e) = result else {
                self.load_job
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .scheduler_push_job(JobPriority::PriorityNone);
                if let Ok(info) = source
                    .query_info_future(
                        gio::FILE_ATTRIBUTE_TIME_MODIFIED,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    let mtime = info
                        .modification_date_time()
                        .map(|datetime| datetime.to_unix())
                        .unwrap_or_default();
                    self.uri_mtime.set(mtime);
                }
                return;
            };
            if e.matches(gio::IOErrorEnum::NotMounted) {
                let operation = gtk::MountOperation::new(
                    self.obj().root().and_downcast_ref::<gtk::Window>(),
                );
                if let Err(e) = source
                    .mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&operation))
                    .await
                {
                    self.load_remote_failed(&e);
                } else {
                    self.load_remote_file(source).await;
                }
            } else if e.matches(gio::IOErrorEnum::Cancelled) {
                self.clear_load_job();
                self.clear_local_uri();
            } else {
                self.load_remote_failed(&e);
            }
        }

        pub(super) fn load_remote_file<'a>(&'a self, file: &'a gio::File) -> LocalBoxFuture<'a, ()> {
            async move {
                if self.local_path.borrow().is_none() {
                    let base_name = self.edit_name.borrow().clone();
                    let template = format!("document.XXXXXX-{base_name}");
                    match papers_document::mkstemp(&template) {
                        Ok((_, temp_file)) => {
                            let f = gio::File::for_path(&temp_file);
                            self.load_job.borrow().as_ref().unwrap().set_uri(&f.uri());
                            self.local_path.replace(Some(temp_file));
                        }
                        Err(e) => {
                            self.show_error_message(
                                Some(&e),
                                &gettext("Failed to Load Remote File."),
                            );
                            return;
                        }
                    }
                }
                let target_file =
                    gio::File::for_path(self.local_path.borrow().as_ref().unwrap());
                self.loader_view.set_uri(file.uri());
                self.set_mode(WindowRunMode::LoaderView);
                let (result, mut stream) = file.copy_future(
                    &target_file,
                    gio::FileCopyFlags::OVERWRITE,
                    glib::Priority::DEFAULT,
                );
                use futures::stream::StreamExt;
                glib::spawn_future_local(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    async move {
                        while let Some((n, total)) = stream.next().await {
                            if total < 0 { return; }
                            obj.loader_view.set_fraction(n as f64 / total as f64);
                        }
                    }
                ));
                let result = result.await;
                self.remote_file_copy_ready(file, result).await;
            }
            .boxed_local()
        }

        fn reload_local(&self) {
            let Some(file) = self
                .local_path
                .borrow()
                .as_ref()
                .map(gio::File::for_path)
                .or(self.file.borrow().clone())
            else {
                return;
            };
            let uri = file.uri();
            let job = JobLoad::new();
            job.set_uri(&uri);
            let id = job.connect_finished(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |job| {
                    if job.is_succeeded().is_err() {
                        obj.clear_reload_job();
                        obj.dest.take();
                        return;
                    }
                    let document = job.loaded_document();
                    obj.document_view.reload_document(document.as_ref());
                    obj.clear_reload_job();
                }
            ));
            job.scheduler_push_job(JobPriority::PriorityNone);
            self.reload_job.replace(Some(job));
            self.reload_job_handler.replace(Some(id));
        }

        async fn copy_and_reload_local(&self, remote: &gio::File) {
            let target = gio::File::for_path(self.local_path.borrow().as_ref().unwrap());
            let (result, _) = remote.copy_future(
                &target,
                gio::FileCopyFlags::OVERWRITE,
                glib::Priority::DEFAULT,
            );
            if let Err(e) = result.await {
                if !e.matches(gio::IOErrorEnum::Cancelled) {
                    self.show_error_message(Some(&e), &gettext("Failed to Reload Document."));
                }
            } else {
                self.reload_local();
            }
        }

        async fn reload_remote(&self) {
            let Some(remote) = self.file.borrow().clone() else { return };
            if let Ok(info) = remote
                .query_info_future(
                    gio::FILE_ATTRIBUTE_TIME_MODIFIED,
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await
                && let Some(time) = info.modification_date_time()
            {
                let mtime = time.to_unix();
                if self.uri_mtime.get() != mtime {
                    self.uri_mtime.set(mtime);
                    self.copy_and_reload_local(&remote).await;
                }
            }
            self.reload_local();
        }

        pub(super) async fn reload_document(&self) {
            self.clear_reload_job();
            self.dest.take();
            if self.local_path.borrow().is_some() {
                self.reload_remote().await;
            } else {
                self.reload_local();
            }
        }

        pub(super) fn file_changed(&self) {
            if !self.check_document_modified_reload() {
                glib::spawn_future_local(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    async move { obj.reload_document().await; }
                ));
            }
        }

        fn check_document_modified_reload(&self) -> bool {
            let Some(secondary_text) = self.obj().check_document_modified() else {
                return false;
            };
            let secondary_text_command =
                gettext("If you reload the document, changes will be permanently lost.");
            let text = gettext("File changed outside Document Viewer. Reload document?");
            let dialog = adw::AlertDialog::builder()
                .body(format!("{secondary_text} {secondary_text_command}"))
                .heading(text)
                .default_response("yes")
                .build();
            dialog.add_responses(&[("no", &gettext("_No")), ("yes", &gettext("_Reload"))]);
            dialog.set_response_appearance("yes", adw::ResponseAppearance::Destructive);
            dialog.connect_response(
                None,
                glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_, response| {
                        if response == "yes" {
                            glib::spawn_future_local(async move { obj.reload_document().await; });
                        }
                    }
                ),
            );
            dialog.present(self.obj().root().and_downcast_ref::<gtk::Window>());
            true
        }

        pub(super) fn open(
            &self,
            file: &impl IsA<gio::File>,
            dest: Option<&papers_document::LinkDest>,
            mode: Option<WindowRunMode>,
        ) {
            self.clear_load_job();
            self.clear_local_uri();
            self.dest.replace(dest.cloned());

            let monitor = PpsFileMonitor::new(&file.uri());
            monitor.connect_closure(
                "changed",
                false,
                closure_local!(
                    #[weak_allow_none(rename_to = obj)]
                    self,
                    move |_: glib::Object| {
                        if let Some(obj) = obj {
                            obj.file_changed();
                        }
                    }
                ),
            );
            self.monitor.replace(Some(monitor));

            let path = file
                .path()
                .and_then(|p| glib::filename_to_uri(p, None).ok());
            let file_uri = file.uri();
            let uri = path.as_ref().map(|gs| gs.as_str()).unwrap_or(file_uri.as_str());

            self.set_filenames(file.as_ref());
            self.init_metadata_with_default_values(file.as_ref());

            let load_job = papers_view::JobLoad::new();
            load_job.set_uri(uri);

            let dest = dest.cloned();
            let settings = gio::Settings::new("org.gnome.Papers");

            let id = load_job.connect_finished(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                #[strong]
                mode,
                move |job| {
                    let uri = obj.obj().uri().unwrap();
                    match job.is_succeeded() {
                        Ok(()) => {
                            let document = job.loaded_document().unwrap();
                            let mut pending_mode = WindowRunMode::Normal;
                            if let Some(info) = document.info()
                                && let Some(page_mode) = info.start_mode()
                                && page_mode == DocumentMode::FullScreen
                            {
                                pending_mode = WindowRunMode::Presentation;
                            }
                            if let Some(m) = mode {
                                pending_mode = m;
                            }
                            obj.document_view.open_document(
                                &document,
                                obj.metadata.borrow().clone().as_ref(),
                                dest.as_ref(),
                                pending_mode,
                            );
                            obj.document_view
                                .set_inverted_colors(settings.boolean("night-mode"));
                            match pending_mode {
                                WindowRunMode::Presentation => obj.run_presentation(),
                                WindowRunMode::Fullscreen => {
                                    if let Some(window) =
                                        obj.obj().root().and_downcast::<crate::window::PpsWindow>()
                                    {
                                        WidgetExt::activate_action(
                                            &window,
                                            "win.fullscreen",
                                            None,
                                        )
                                        .unwrap();
                                    }
                                    obj.set_mode(pending_mode);
                                }
                                _ => obj.set_mode(pending_mode),
                            }
                            gtk::RecentManager::default().add_item(&uri);
                            #[cfg(feature = "with-keyring")]
                            if let Some(password) = job.password() {
                                let flags = job.password_save();
                                glib::spawn_future_local(async move {
                                    if let Err(e) =
                                        crate::keyring::save_password(&uri, &password, flags).await
                                    {
                                        glib::g_warning!("", "Failed to save password: {e}");
                                    }
                                });
                            }
                            obj.clear_load_job();
                        }
                        Err(e) => {
                            glib::spawn_future_local(glib::clone!(
                                #[weak]
                                job,
                                async move {
                                    if e.matches(papers_document::DocumentError::Encrypted) {
                                        obj.set_mode(WindowRunMode::PasswordView);
                                        #[cfg(feature = "with-keyring")]
                                        match crate::keyring::lookup_password(&uri).await {
                                            Ok(Some(password)) => {
                                                if job.password().is_some_and(|p| p == password) {
                                                    job.set_password(None);
                                                } else {
                                                    job.set_password(Some(&password));
                                                    job.scheduler_push_job(JobPriority::PriorityNone);
                                                    return;
                                                }
                                            }
                                            Ok(None) => {}
                                            Err(e) => {
                                                glib::g_warning!("", "Failed to lookup password: {e}");
                                            }
                                        }
                                        let wrong_password = job.password().is_some();
                                        job.set_password(None);
                                        obj.password_view
                                            .set_filename(obj.display_name.borrow().as_str());
                                        obj.password_view.ask_password(wrong_password);
                                        obj.set_mode(WindowRunMode::PasswordView);
                                    } else {
                                        obj.show_error(Some(&e));
                                        obj.clear_load_job();
                                        obj.clear_local_uri();
                                    }
                                }
                            ));
                        }
                    }
                }
            ));

            self.load_job.replace(Some(load_job.clone()));
            self.load_job_handler.replace(Some(id));
            self.file.replace(Some(file.clone().into()));

            let file = file.clone();
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                async move {
                    let is_gvfs = file
                        .uri_scheme()
                        .map(|scheme| {
                            matches!(
                                scheme.as_str(),
                                "afc" | "afp" | "dav" | "davs" | "ftp" | "google-drive"
                                    | "mtp" | "nfs" | "onedrive" | "sftp" | "smb"
                            )
                        })
                        .unwrap_or(false);
                    if path.is_none() || is_gvfs {
                        obj.load_remote_file(file.as_ref()).await;
                    } else {
                        let result = file.read_future(glib::Priority::DEFAULT).await;
                        match result {
                            Ok(stream) => {
                                if !stream.can_seek() {
                                    return obj.load_remote_file(file.as_ref()).await;
                                }
                                let seekable = stream.upcast_ref::<gio::Seekable>();
                                if seekable
                                    .seek(0, glib::SeekType::End, gio::Cancellable::NONE)
                                    .is_err()
                                {
                                    return obj.load_remote_file(file.as_ref()).await;
                                }
                                obj.loader_view.set_fraction(-1_f64);
                                obj.set_mode(WindowRunMode::LoaderView);
                                load_job.scheduler_push_job(papers_view::JobPriority::PriorityNone);
                            }
                            Err(_) => obj.load_remote_file(file.as_ref()).await,
                        }
                    }
                }
            ));
        }

        pub(super) fn run_presentation(&self) {
            let model = self.document_view.model();
            let Some(document) = model.document() else { return };
            if let Some(window) = self.obj().root().and_downcast::<crate::window::PpsWindow>() {
                if let Some(state) = window.action_state("fullscreen")
                    && state.get::<bool>().unwrap_or_default()
                {
                    window.change_action_state("fullscreen", &false.into());
                }
                window
                    .lookup_action("fullscreen")
                    .and_downcast::<gio::SimpleAction>()
                    .unwrap()
                    .set_enabled(false);
            }
            self.presentation.set_document(Some(&document));
            self.presentation.set_current_page(model.page() as u32);
            self.presentation.set_rotation(model.rotation());
            self.presentation.set_inverted_colors(model.is_inverted_colors());
            self.set_mode(WindowRunMode::Presentation);
            self.presentation.grab_focus();
            if let Some(window) = self.obj().root().and_downcast::<gtk::Window>() {
                window.fullscreen();
            }
        }

        pub(super) fn stop_presentation(&self) {
            let page = self.presentation.current_page();
            let rotation = self.presentation.rotation();
            let model = self.document_view.model();
            model.set_page(page as i32);
            model.set_rotation(rotation as i32);
            if let Some(window) = self.obj().root().and_downcast::<crate::window::PpsWindow>() {
                window
                    .lookup_action("fullscreen")
                    .and_downcast::<gio::SimpleAction>()
                    .unwrap()
                    .set_enabled(true);
            }
            self.presentation.set_document(papers_document::Document::NONE);
            self.set_mode(WindowRunMode::Normal);
            if let Some(window) = self.obj().root().and_downcast::<gtk::Window>() {
                window.unfullscreen();
            }
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

    pub fn open(
        &self,
        file: &gio::File,
        dest: Option<&papers_document::LinkDest>,
        mode: Option<WindowRunMode>,
    ) {
        self.imp().open(file, dest, mode);
    }

    pub fn run_presentation(&self) {
        self.imp().run_presentation();
    }

    pub fn stop_presentation(&self) {
        self.imp().stop_presentation();
    }

    pub fn mode(&self) -> WindowRunMode {
        self.imp().mode.get()
    }

    pub fn set_mode(&self, mode: WindowRunMode) {
        self.imp().set_mode(mode);
    }

    pub fn metadata(&self) -> Option<papers_view::Metadata> {
        self.imp().metadata.borrow().clone()
    }
}

impl Default for PpsTab {
    fn default() -> Self {
        Self::new()
    }
}
