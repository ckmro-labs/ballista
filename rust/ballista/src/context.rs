// Copyright 2020 Andy Grove
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Distributed execution context.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::client::BallistaClient;
use crate::error::{BallistaError, Result};
use crate::serde::scheduler::Action;

use datafusion::dataframe::DataFrame;
use datafusion::execution::context::ExecutionContext;
use datafusion::logical_plan::{DFSchema, Expr, LogicalPlan, Partitioning};
use datafusion::physical_plan::csv::CsvReadOptions;
use datafusion::physical_plan::SendableRecordBatchStream;
use log::info;

#[derive(Debug)]
pub enum ClusterMeta {
    Direct { host: String, port: usize }, //TODO add etcd and k8s options here
}

#[allow(dead_code)]
pub struct BallistaContextState {
    /// Meta-data required for connecting to a scheduler instances in the cluster
    cluster_meta: ClusterMeta,
    /// General purpose settings
    settings: HashMap<String, String>, // map from shuffle id to executor uuid
                                       // shuffle_locations: HashMap<ShuffleId, ExecutorMeta>,
                                       // config: ExecutorConfig
}

impl BallistaContextState {
    pub fn new(cluster_meta: ClusterMeta, settings: HashMap<String, String>) -> Self {
        Self {
            cluster_meta,
            settings,
        }
    }
}

#[allow(dead_code)]
pub struct BallistaContext {
    state: Arc<Mutex<BallistaContextState>>,
}

impl BallistaContext {
    /// Create a context for executing queries against a remote Ballista executor instance
    pub fn remote(host: &str, port: usize, settings: HashMap<&str, &str>) -> Self {
        let meta = ClusterMeta::Direct {
            host: host.to_owned(),
            port,
        };
        let settings: HashMap<String, String> = settings
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let state = BallistaContextState::new(meta, settings);
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Create a DataFrame representing a Parquet table scan
    pub fn read_parquet(&self, path: &str) -> Result<BallistaDataFrame> {
        // use local DataFusion context for now but later this might call the scheduler
        let mut ctx = ExecutionContext::new();
        let df = ctx.read_parquet(path)?;
        Ok(BallistaDataFrame::from(self.state.clone(), df))
    }

    /// Create a DataFrame representing a CSV table scan
    pub fn read_csv(&self, path: &str, options: CsvReadOptions) -> Result<BallistaDataFrame> {
        // use local DataFusion context for now but later this might call the scheduler
        let mut ctx = ExecutionContext::new();
        let df = ctx.read_csv(path, options)?;
        Ok(BallistaDataFrame::from(self.state.clone(), df))
    }

    /// Register a DataFrame as a table that can be referenced from a SQL query
    pub fn register_table(&self, _name: &str, _table: Arc<dyn DataFrame>) -> Result<()> {
        todo!()
    }

    /// Create a DataFrame from a SQL statement
    pub fn sql(&self, _sql: &str) -> Result<BallistaDataFrame> {
        todo!()
    }
}

/// The Ballista DataFrame is a wrapper around the DataFusion DataFrame and overrides the
/// `collect` method so that the query is executed against Ballista and not DataFusion.
pub struct BallistaDataFrame {
    /// Ballista context state
    state: Arc<Mutex<BallistaContextState>>,
    /// DataFusion DataFrame representing logical query plan
    df: Arc<dyn DataFrame>,
}

impl BallistaDataFrame {
    pub fn from(state: Arc<Mutex<BallistaContextState>>, df: Arc<dyn DataFrame>) -> Self {
        Self { state, df }
    }

    pub async fn collect(&self) -> Result<SendableRecordBatchStream> {
        let (host, port) = {
            let state = self.state.lock().unwrap();
            match &state.cluster_meta {
                ClusterMeta::Direct { host, port, .. } => (host.to_owned(), *port),
            }
        };
        info!("Connecting to Ballista executor at {}:{}", host, port);
        let mut client = BallistaClient::try_new(&host, port).await?;
        client
            .execute_action(&Action::InteractiveQuery {
                plan: self.df.to_logical_plan(),
                settings: Default::default(),
            })
            .await
    }

    pub fn select_columns(&self, columns: Vec<&str>) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df
                .select_columns(columns)
                .map_err(BallistaError::from)?,
        ))
    }

    pub fn select(&self, expr: Vec<Expr>) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df.select(expr).map_err(BallistaError::from)?,
        ))
    }

    pub fn filter(&self, expr: Expr) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df.filter(expr).map_err(BallistaError::from)?,
        ))
    }

    pub fn aggregate(
        &self,
        group_expr: Vec<Expr>,
        aggr_expr: Vec<Expr>,
    ) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df
                .aggregate(group_expr, aggr_expr)
                .map_err(BallistaError::from)?,
        ))
    }

    pub fn limit(&self, n: usize) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df.limit(n).map_err(BallistaError::from)?,
        ))
    }

    pub fn sort(&self, expr: Vec<Expr>) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df.sort(expr).map_err(BallistaError::from)?,
        ))
    }

    // TODO lifetime issue
    // pub fn join(&self, right: Arc<dyn DataFrame>, join_type: JoinType, left_cols: &[&str], right_cols: &[&str]) -> Result<BallistaDataFrame> {
    //     Ok(Self::from(self.state.clone(), self.df.join(right, join_type, &left_cols, &right_cols).map_err(BallistaError::from)?))
    // }

    pub fn repartition(&self, partitioning_scheme: Partitioning) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df
                .repartition(partitioning_scheme)
                .map_err(BallistaError::from)?,
        ))
    }

    pub fn schema(&self) -> &DFSchema {
        self.df.schema()
    }

    pub fn to_logical_plan(&self) -> LogicalPlan {
        self.df.to_logical_plan()
    }

    pub fn explain(&self, verbose: bool) -> Result<BallistaDataFrame> {
        Ok(Self::from(
            self.state.clone(),
            self.df.explain(verbose).map_err(BallistaError::from)?,
        ))
    }
}

// #[async_trait]
// impl ExecutionContext for BallistaContext {
//     async fn get_executor_ids(&self) -> Result<Vec<ExecutorMeta>> {
//         match &self.config.discovery_mode {
//             DiscoveryMode::Etcd => etcd_get_executors(&self.config.etcd_urls, "default").await,
//             DiscoveryMode::Kubernetes => k8s_get_executors("default", "ballista").await,
//             DiscoveryMode::Standalone => Err(ballista_error("Standalone mode not implemented yet")),
//         }
//     }
//
//     async fn execute_task(
//         &self,
//         executor_meta: ExecutorMeta,
//         task: ExecutionTask,
//     ) -> Result<ShuffleId> {
//         // TODO what is the point of returning this info since it is based on input arg?
//         let shuffle_id = ShuffleId::new(task.job_uuid, task.stage_id, task.partition_id);
//
//         let _ = execute_action(
//             &executor_meta.host,
//             executor_meta.port,
//             &Action::Execute(task),
//         )
//         .await?;
//
//         Ok(shuffle_id)
//     }
//
//     async fn read_shuffle(&self, shuffle_id: &ShuffleId) -> Result<Vec<ColumnarBatch>> {
//         match self.shuffle_locations.get(shuffle_id) {
//             Some(executor_meta) => {
//                 let batches = execute_action(
//                     &executor_meta.host,
//                     executor_meta.port,
//                     &Action::FetchShuffle(*shuffle_id),
//                 )
//                 .await?;
//                 Ok(batches
//                     .iter()
//                     .map(|b| ColumnarBatch::from_arrow(b))
//                     .collect())
//             }
//             _ => Err(ballista_error(&format!(
//                 "Failed to resolve executor UUID for shuffle ID {:?}",
//                 shuffle_id
//             ))),
//         }
//     }
//
//     fn config(&self) -> ExecutorConfig {
//         self.config.clone()
//     }
// }
