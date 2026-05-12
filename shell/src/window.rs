use crate::deps::*;

use crate::config::PROFILE;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum WindowRunMode {
    #[default]
    StartView,
    Normal,
    Fullscreen,
    Presentation,
    LoaderView,
    ErrorView,
    PasswordView,
}

mod imp {
    use super::*;

    #[derive(Default, Debug, CompositeTemplate)]
    #[template(resource = "/org/gnome/papers/ui/window.ui")]
    pub struct PpsWindow {
        #[template_child]
        pub(super) stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub(super) tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        pub(super) tab_bar: TemplateChild<adw::TabBar>,
        #[template_child]
        pub(super) tab_overview: TemplateChild<adw::TabOverview>,
        #[template_child]
        pub(super) settings: TemplateChild<gio::Settings>,
        #[template_child]
        pub(super) default_settings: TemplateChild<gio::Settings>,
        #[template_child]
        pub(super) toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub(super) error_alert: TemplateChild<adw::AlertDialog>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PpsWindow {
        const NAME: &'static str = "PpsWindow";
        type Type = super::PpsWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            PpsTab::ensure_type();
            klass.bind_template();
            klass.bind_template_callbacks();
            klass.set_accessible_role(gtk::AccessibleRole::Window);
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PpsWindow {
        fn constructed(&self) {
            self.parent_constructed();
            #[allow(clippy::const_is_empty)]
            if !PROFILE.is_empty() {
                self.obj().add_css_class("devel");
            }
            self.setup_actions();
            self.obj()
                .change_action_state("night-mode", &self.settings.boolean("night-mode").into());

            // Hide tab bar when ≤ 1 tab
            self.tab_view.bind_property("n-pages", &self.tab_bar.get(), "visible")
                .transform_to(|_, n: i32| Some(n > 1))
                .sync_create()
                .build();

            // Close window when last tab is closed
            self.tab_view.connect_close_page(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |tab_view, page| {
                    let tab = page.child().downcast::<PpsTab>().unwrap();
                    if let Some(secondary) = tab.check_document_modified() {
                        let primary = gettext("Close document?");
                        let dialog = adw::AlertDialog::builder()
                            .heading(&primary)
                            .body(&secondary)
                            .default_response("close")
                            .build();
                        dialog.add_responses(&[
                            ("cancel", &gettext("_Cancel")),
                            ("close", &gettext("_Close")),
                        ]);
                        dialog.set_response_appearance(
                            "close",
                            adw::ResponseAppearance::Destructive,
                        );
                        dialog.connect_response(
                            None,
                            glib::clone!(
                                #[weak]
                                tab_view,
                                #[weak]
                                page,
                                move |_, response| {
                                    if response == "close" {
                                        tab_view.close_page_finish(&page, true);
                                        if tab_view.n_pages() == 0 {
                                            if let Some(window) = tab_view
                                                .root()
                                                .and_downcast::<super::PpsWindow>()
                                            {
                                                window.close();
                                            }
                                        }
                                    } else {
                                        tab_view.close_page_finish(&page, false);
                                    }
                                }
                            ),
                        );
                        dialog.present(
                            obj.obj()
                                .root()
                                .and_downcast_ref::<gtk::Window>(),
                        );
                        return glib::Propagation::Stop; // handled asynchronously
                    }
                    // No unsaved changes — allow close and close window if last tab
                    tab_view.close_page_finish(page, true);
                    if tab_view.n_pages() == 0 {
                        obj.obj().close();
                    }
                    glib::Propagation::Proceed
                }
            ));

            // Update window title when active tab changes
            self.tab_view.connect_notify_local(
                Some("selected-page"),
                glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_, _| obj.sync_title_from_active_tab()
                ),
            );
        }

        fn dispose(&self) {
            self.default_settings.apply();
        }
    }

    impl WidgetImpl for PpsWindow {}

    impl WindowImpl for PpsWindow {
        fn close_request(&self) -> glib::Propagation {
            // Check all tabs for unsaved changes
            for i in 0..self.tab_view.n_pages() {
                let page = self.tab_view.nth_page(i);
                let tab = page.child().downcast::<PpsTab>().unwrap();
                if tab.close_handled() == glib::Propagation::Stop {
                    return glib::Propagation::Stop;
                }
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for PpsWindow {}
    impl AdwApplicationWindowImpl for PpsWindow {}

    impl PpsWindow {
        // ── Tab management ──────────────────────────────────────────────

        pub(super) fn active_tab(&self) -> Option<PpsTab> {
            self.tab_view
                .selected_page()
                .and_then(|p| p.child().downcast::<PpsTab>().ok())
        }

        pub(super) fn new_tab(&self) -> PpsTab {
            let tab = PpsTab::new();
            let page = self.tab_view.add_page(&tab, None);

            // Bind tab's display-name to the tab page title
            tab.bind_property("display-name", &page, "title")
                .sync_create()
                .build();

            // Switch to has-tabs view on first tab
            if self.tab_view.n_pages() == 1 {
                self.stack.set_visible_child_name("has-tabs");
                self.setup_window_size();
            }

            self.tab_view.set_selected_page(&page);
            tab
        }

        pub(super) fn close_tab(&self, tab: &PpsTab) {
            let page = self.tab_view.page(tab);
            self.tab_view.close_page(&page);
        }

        fn sync_title_from_active_tab(&self) {
            let title = self
                .active_tab()
                .map(|t| t.display_name())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| gettext("Document Viewer"));
            self.obj().set_title(Some(&title));
        }

        // ── Actions ─────────────────────────────────────────────────────

        fn setup_actions(&self) {
            let actions = [
                gio::ActionEntryBuilder::new("open")
                    .activate(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _| obj.cmd_file_open()
                    ))
                    .build(),
                gio::ActionEntryBuilder::new("new-tab")
                    .activate(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _| obj.cmd_file_open()
                    ))
                    .build(),
                gio::ActionEntryBuilder::new("close-tab")
                    .activate(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _| {
                            if let Some(tab) = obj.active_tab() {
                                obj.close_tab(&tab);
                            }
                        }
                    ))
                    .build(),
                gio::ActionEntryBuilder::new("close")
                    .activate(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _| obj.obj().close()
                    ))
                    .build(),
                gio::ActionEntryBuilder::new("fullscreen")
                    .state(false.into())
                    .change_state(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, action, state| {
                            let enabled = state.and_then(|v| v.get::<bool>()).unwrap();
                            if let Some(tab) = obj.active_tab() {
                                tab.document_view().set_fullscreen_mode(enabled);
                            }
                            if enabled {
                                obj.obj().fullscreen();
                            } else {
                                obj.obj().unfullscreen();
                            }
                            action.set_state(state.unwrap());
                        }
                    ))
                    .build(),
                gio::ActionEntryBuilder::new("escape")
                    .activate(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _| obj.cmd_escape()
                    ))
                    .build(),
                gio::ActionEntryBuilder::new("night-mode")
                    .state(false.into())
                    .change_state(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, action, state| {
                            let state = state.unwrap();
                            let night_mode = state.get::<bool>().unwrap();
                            action.set_state(state);
                            obj.set_night_mode(night_mode);
                        }
                    ))
                    .build(),
                gio::ActionEntryBuilder::new("presentation")
                    .activate(glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _, _| obj.cmd_presentation()
                    ))
                    .build(),
            ];
            self.obj().add_action_entries(actions);

            // Tab navigation shortcuts
            let next = gtk::ShortcutController::new();
            next.set_scope(gtk::ShortcutScope::Managed);
            next.add_shortcut(gtk::Shortcut::new(
                Some(gtk::ShortcutTrigger::parse_string("<Ctrl>Tab").unwrap()),
                Some(gtk::CallbackAction::new(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    #[upgrade_or]
                    glib::Propagation::Proceed,
                    move |_, _| {
                        let tv = &obj.tab_view;
                        if let Some(page) = tv.selected_page() {
                            let idx = (tv.page_position(&page) + 1) % tv.n_pages();
                            tv.set_selected_page(&tv.nth_page(idx));
                        }
                        glib::Propagation::Stop
                    }
                ))),
            ));
            self.obj().add_controller(next);

            let prev = gtk::ShortcutController::new();
            prev.set_scope(gtk::ShortcutScope::Managed);
            prev.add_shortcut(gtk::Shortcut::new(
                Some(gtk::ShortcutTrigger::parse_string("<Ctrl><Shift>Tab").unwrap()),
                Some(gtk::CallbackAction::new(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    #[upgrade_or]
                    glib::Propagation::Proceed,
                    move |_, _| {
                        let tv = &obj.tab_view;
                        if let Some(page) = tv.selected_page() {
                            let n = tv.n_pages();
                            let idx = (tv.page_position(&page) + n - 1) % n;
                            tv.set_selected_page(&tv.nth_page(idx));
                        }
                        glib::Propagation::Stop
                    }
                ))),
            ));
            self.obj().add_controller(prev);
        }

        fn setup_window_size(&self) {
            let window = self.obj().clone();
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(100),
                glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move || {
                        obj.default_settings.delay();
                        obj.default_settings
                            .bind("window-width", &window, "default-width")
                            .build();
                        obj.default_settings
                            .bind("window-height", &window, "default-height")
                            .build();
                        obj.default_settings
                            .bind("window-maximized", &window, "maximized")
                            .build();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(100),
                            glib::clone!(
                                #[weak]
                                obj,
                                move || {
                                    if let Some(tab) = obj.active_tab() {
                                        tab.document_view().focus_view();
                                    }
                                }
                            ),
                        );
                    }
                ),
            );
        }

        fn cmd_escape(&self) {
            if let Some(tab) = self.active_tab() {
                if tab.mode() == WindowRunMode::Presentation {
                    tab.stop_presentation();
                    return;
                }
                tab.document_view()
                    .activate_action("doc.escape", None)
                    .unwrap();
            }
        }

        fn cmd_presentation(&self) {
            if let Some(tab) = self.active_tab() {
                if tab.mode() != WindowRunMode::Presentation {
                    tab.run_presentation();
                }
            }
        }

        fn set_night_mode(&self, night_mode: bool) {
            // Apply to all open tabs
            for i in 0..self.tab_view.n_pages() {
                let page = self.tab_view.nth_page(i);
                if let Ok(tab) = page.child().downcast::<PpsTab>() {
                    tab.document_view().set_inverted_colors(night_mode);
                }
            }
            let manager =
                adw::StyleManager::for_display(&WidgetExt::display(&self.obj().clone()));
            manager.set_color_scheme(if night_mode {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::Default
            });
            self.settings
                .set_boolean("night-mode", night_mode)
                .expect("failed to save night-mode");
        }

        // Error display (called by PpsTab children via root-window lookup)
        pub(super) fn show_error_message(&self, error: Option<&glib::Error>, msg: &str) {
            let toast = adw::Toast::builder().timeout(20).title(msg).build();
            if let Some(error) = error {
                toast.set_button_label(Some(&gettext("View Details")));
                toast.connect_button_clicked(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_| obj.error_alert.present(Some(obj.obj().as_ref()))
                ));
                self.error_alert.set_heading(Some(msg));
                self.error_alert.set_body(error.message());
            }
            self.toast_overlay.add_toast(toast);
        }

        fn file_dialog_restore_folder(
            &self,
            dialog: &gtk::FileDialog,
            dir: glib::UserDirectory,
        ) {
            let settings = self.settings.get();
            let key = Self::settings_key_for_directory(dir);
            let folder = settings
                .get::<Option<String>>(&key)
                .map(std::path::PathBuf::from);
            let folder = folder
                .or_else(|| glib::user_special_dir(dir))
                .unwrap_or_else(glib::home_dir);
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }

        fn file_dialog_save_folder(
            &self,
            file: Option<&gio::File>,
            dir: glib::UserDirectory,
        ) {
            let folder = file.and_then(|f| f.parent());
            let path = folder
                .filter(|f| f.path() != glib::user_special_dir(dir))
                .and_then(|f| f.path())
                .and_then(|path| path.into_os_string().into_string().ok());
            let settings = self.settings.get();
            let key = Self::settings_key_for_directory(dir);
            settings.set(&key, path).expect("Failed to save folder path");
        }

        fn settings_key_for_directory(dir: glib::UserDirectory) -> String {
            match dir {
                glib::UserDirectory::Pictures => "pictures-directory",
                _ => "document-directory",
            }
            .into()
        }

        fn cmd_file_open(&self) {
            let dialog = gtk::FileDialog::builder().modal(true).build();
            papers_document::Document::factory_add_filters(
                &dialog,
                papers_document::Document::NONE,
            );
            self.file_dialog_restore_folder(&dialog, glib::UserDirectory::Documents);
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                async move {
                    let Ok(files) =
                        dialog.open_multiple_future(Some(obj.obj().as_ref())).await
                    else {
                        return;
                    };
                    for f in files.iter::<gio::File>() {
                        let f = f.unwrap();
                        let tab = obj.new_tab();
                        tab.open(&f, None, None);
                    }
                    if files.n_items() > 0 {
                        let file = files.item(0).and_downcast::<gio::File>();
                        obj.file_dialog_save_folder(
                            file.as_ref(),
                            glib::UserDirectory::Documents,
                        );
                    }
                }
            ));
        }
    }

    // Template callbacks MUST be in a separate impl block with this attribute
    // so that bind_template_callbacks() in class_init can find them.
    #[gtk::template_callbacks]
    impl PpsWindow {
        #[template_callback]
        fn window_fullscreened(&self) {
            if !self.obj().is_fullscreen() {
                self.obj().change_action_state("fullscreen", &false.into());
            }
        }

        #[template_callback]
        fn night_mode_changed(&self) {
            let night_mode = self.settings.boolean("night-mode");
            let current = self
                .obj()
                .action_state("night-mode")
                .unwrap()
                .get::<bool>()
                .unwrap();
            if night_mode != current {
                self.obj()
                    .change_action_state("night-mode", &night_mode.into());
            }
        }

        #[template_callback]
        fn drag_data_received(&self, value: glib::BoxedValue) -> bool {
            let Ok(file_list) = value.get_owned::<gdk::FileList>() else {
                return false;
            };
            for file in file_list.files() {
                let tab = self.new_tab();
                tab.open(&file, None, None);
            }
            true
        }
    }
}

glib::wrapper! {
    pub struct PpsWindow(ObjectSubclass<imp::PpsWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gtk::Native, gio::ActionGroup, gio::ActionMap, gtk::Accessible,
                    gtk::Buildable, gtk::ConstraintTarget, gtk::Root, gtk::ShortcutManager;
}

impl PpsWindow {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application", gio::Application::default())
            .property("show-menubar", false)
            .build()
    }

    pub fn tab_view(&self) -> adw::TabView {
        self.imp().tab_view.get()
    }

    pub fn uri(&self) -> Option<String> {
        self.imp().active_tab().and_then(|t| t.uri())
    }

    pub fn open(
        &self,
        file: &gio::File,
        dest: Option<&papers_document::LinkDest>,
        mode: Option<WindowRunMode>,
    ) {
        let tab = self.imp().new_tab();
        tab.open(file, dest, mode);
    }

    pub fn document(&self) -> Option<papers_document::Document> {
        self.imp()
            .active_tab()
            .and_then(|t| t.document_view().model().document())
    }

    pub fn is_empty(&self) -> bool {
        self.imp().tab_view.n_pages() == 0
            || self
                .imp()
                .active_tab()
                .map(|t| t.is_empty())
                .unwrap_or(true)
    }

    pub fn metadata(&self) -> Option<papers_view::Metadata> {
        self.imp().active_tab().and_then(|t| t.metadata())
    }

    pub fn print_range(&self, first_page: i32, last_page: i32) {
        if let Some(tab) = self.imp().active_tab() {
            tab.document_view().print_range(first_page, last_page);
        }
    }

    pub fn open_copy(
        &self,
        metadata: Option<&papers_view::Metadata>,
        dest: Option<&papers_document::LinkDest>,
        display_name: &str,
        edit_name: &str,
    ) {
        let win = PpsWindow::new();
        let tab = win.imp().new_tab();
        let Some(document) = self
            .imp()
            .active_tab()
            .and_then(|t| t.document_view().model().document())
        else {
            return;
        };
        tab.document_view().set_filenames(display_name, edit_name);
        tab.document_view().open_document(
            &document,
            metadata,
            dest,
            WindowRunMode::Normal,
        );
        tab.set_mode(WindowRunMode::Normal);
        win.set_default_size(self.width(), self.height());
        win.present();
    }

    pub(crate) fn show_error_message(&self, error: Option<&glib::Error>, msg: &str) {
        self.imp().show_error_message(error, msg);
    }
}

impl Default for PpsWindow {
    fn default() -> Self {
        Self::new()
    }
}
