//! Views display confirmed Origin Hosts only; Capture presence is Provenance.
use crate::archive::db::SessionRow;
pub(super) fn display(session: &SessionRow) -> String {
    session
        .host_attribution
        .as_ref()
        .filter(|attribution| {
            attribution.status == crate::origins::AttributionStatus::Confirmed
                && !crate::adapters::is_hosted_source(&session.source)
        })
        .map(|attribution| attribution.host.clone())
        .unwrap_or_default()
}
