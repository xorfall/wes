//! Status evidence only: no source text, values, names or provider details.
macro_rules! reasons {
    ($($variant:ident => ($code:literal, $message:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum StaleReason { $($variant),+ }
        impl StaleReason {
            pub fn code(self) -> &'static str { match self { $(Self::$variant => $code),+ } }
            pub fn message(self) -> &'static str { match self { $(Self::$variant => $message),+ } }
            pub fn from_code(code: &str) -> Option<Self> {
                match code { $($code => Some(Self::$variant)),+, _ => None }
            }
        }
    }
}
reasons! {
    Unknown => ("unknown", "The reason this result became stale was not recorded."),
    DefinitionChanged => ("definition_changed", "The definition changed; the previous result is no longer current."),
    DependencyChanged => ("dependency_changed", "An upstream definition changed; this result needs to be recalculated."),
    RefreshRequested => ("refresh_requested", "A new run was requested; the previous result is no longer current."),
    DependencyRefreshed => ("dependency_refreshed", "An upstream result was requested again; this result is no longer current."),
    InputBehind => ("input_behind", "The calculation completed an older input while newer input arrived; waiting to compute the latest committed input."),
    StreamUpdated => ("stream_updated", "An upstream stream changed its data or availability; this result is no longer current."),
    ResultEvicted => ("result_evicted", "The result was removed from live memory and is no longer available to this cell."),
    ResultWithdrawn => ("result_withdrawn", "Access to the result was withdrawn. Cached data and value-derived metadata were cleared; no work was replayed."),
    RestoreNotRetained => ("restore_not_retained", "No retained result was available when the workspace reopened. The command was not rerun."),
    RestoreUnavailable => ("restore_unavailable", "The retained result could not be loaded when the workspace reopened. The command was not rerun."),
    RestoreUnfinished => ("restore_unfinished", "No completed result was recorded before the workspace reopened. Check run history before retrying; external effects may already have occurred."),
    RestoreChanged => ("restore_changed", "A definition or dependency changed; the previous result is no longer current. Reopening preserved this stale state and did not rerun the command."),
}
