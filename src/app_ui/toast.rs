use crate::AppWindow;

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastKind {
    fn as_i32(self) -> i32 {
        match self {
            Self::Info => 0,
            Self::Success => 1,
            Self::Warning => 2,
            Self::Error => 3,
        }
    }
}

pub(crate) fn show(
    app: &AppWindow,
    kind: ToastKind,
    title: impl AsRef<str>,
    body: impl AsRef<str>,
) {
    app.invoke_show_toast(kind.as_i32(), title.as_ref().into(), body.as_ref().into());
}

pub(crate) fn show_with_action(
    app: &AppWindow,
    kind: ToastKind,
    title: impl AsRef<str>,
    body: impl AsRef<str>,
    action_label: impl AsRef<str>,
) {
    app.invoke_show_toast_action(
        kind.as_i32(),
        title.as_ref().into(),
        body.as_ref().into(),
        action_label.as_ref().into(),
    );
}
