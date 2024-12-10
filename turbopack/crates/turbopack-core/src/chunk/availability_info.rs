use anyhow::Result;
use turbo_tasks::{ResolvedVc, Vc};

use super::available_modules::{AvailableModuleInfoMap, AvailableModules};

#[turbo_tasks::value(serialization = "auto_for_input")]
#[derive(Hash, Clone, Copy, Debug)]
pub enum AvailabilityInfo {
    /// Availability of modules is not tracked
    Untracked,
    /// Availablility of modules is tracked, but no modules are available
    Root,
    /// There are modules already available.
    Complete {
        available_chunk_items: ResolvedVc<AvailableModules>,
    },
}

impl AvailabilityInfo {
    pub fn available_chunk_items(&self) -> Option<Vc<AvailableModules>> {
        match self {
            Self::Untracked => None,
            Self::Root => None,
            Self::Complete {
                available_chunk_items,
                ..
            } => Some(**available_chunk_items),
        }
    }

    pub async fn with_chunk_items(self, chunk_items: Vc<AvailableModuleInfoMap>) -> Result<Self> {
        Ok(match self {
            AvailabilityInfo::Untracked => AvailabilityInfo::Untracked,
            AvailabilityInfo::Root => AvailabilityInfo::Complete {
                available_chunk_items: AvailableModules::new(chunk_items).to_resolved().await?,
            },
            AvailabilityInfo::Complete {
                available_chunk_items,
            } => AvailabilityInfo::Complete {
                available_chunk_items: available_chunk_items
                    .with_chunk_items(chunk_items)
                    .to_resolved()
                    .await?,
            },
        })
    }
}
