use anyhow::{Context as _, Result};

pub(super) fn create_overlay_analytical_views(conn: &duckdb::Connection) -> Result<()> {
    let has_base_centrality = base_relation_exists(conn, "v_symbol_centrality")?;
    let has_base_component = base_relation_exists(conn, "v_symbol_component")?;
    let has_base_community = base_relation_exists(conn, "v_symbol_community")?;
    let has_base_metrics = base_relation_exists(conn, "v_graph_metrics")?;
    let has_base_symbol_file = base_relation_exists(conn, "v_symbol_file")?;
    let has_base_churn = base_relation_exists(conn, "v_symbol_churn_90d")?;
    let has_base_blast_radius = base_relation_exists(conn, "v_blast_radius")?;
    let has_base_commit_files = base_relation_exists(conn, "v_commit_files")?;
    let has_base_file_cochange = base_relation_exists(conn, "v_file_cochange")?;
    let has_base_commit_classified = base_relation_exists(conn, "v_commit_classified")?;
    let has_base_fix_hotspots = base_relation_exists(conn, "v_fix_hotspots")?;
    let has_base_hidden_coupling = base_relation_exists(conn, "v_hidden_coupling")?;
    let has_base_velocity = base_relation_exists(conn, "v_velocity")?;
    let has_base_age = base_relation_exists(conn, "v_symbol_age")?;
    let has_base_genealogy = base_relation_exists(conn, "v_symbol_genealogy")?;

    let centrality_join = if has_base_centrality {
        "LEFT JOIN base.v_symbol_centrality base_ct
          ON base_ct.stable_symbol_id = n.stable_symbol_id"
    } else {
        ""
    };
    let pagerank_expr = if has_base_centrality {
        "COALESCE(base_ct.pagerank, 0.0)"
    } else {
        "0.0"
    };

    let component_join = if has_base_component {
        "LEFT JOIN base.v_symbol_component base_cmp
          ON base_cmp.stable_symbol_id = n.stable_symbol_id"
    } else {
        ""
    };
    let component_id_expr = if has_base_component {
        "COALESCE(base_cmp.component_id, -n.node_id - 1)"
    } else {
        "-n.node_id - 1"
    };
    let component_size_expr = if has_base_component {
        "COALESCE(base_cmp.component_size, 1)"
    } else {
        "1"
    };

    let community_join = if has_base_community {
        "LEFT JOIN base.v_symbol_community base_comm
          ON base_comm.stable_symbol_id = n.stable_symbol_id"
    } else {
        ""
    };
    let community_id_expr = if has_base_community {
        "COALESCE(base_comm.community_id, -n.node_id - 1)"
    } else {
        "-n.node_id - 1"
    };

    let components_expr = if has_base_metrics {
        "(SELECT components FROM base.v_graph_metrics LIMIT 1)"
    } else {
        "CAST(NULL AS BIGINT)"
    };
    let largest_component_expr = if has_base_metrics {
        "(SELECT largest_component FROM base.v_graph_metrics LIMIT 1)"
    } else {
        "CAST(NULL AS BIGINT)"
    };
    let communities_expr = if has_base_metrics {
        "(SELECT communities FROM base.v_graph_metrics LIMIT 1)"
    } else {
        "CAST(NULL AS BIGINT)"
    };

    let symbol_file_sql = if has_base_symbol_file {
        "SELECT sf.*
         FROM base.v_symbol_file sf"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS VARCHAR) AS stable_symbol_id,
           CAST(NULL AS VARCHAR) AS commit_sha,
           CAST(NULL AS VARCHAR) AS file_path
         WHERE false"
            .to_owned()
    };

    let churn_join = if has_base_churn {
        "LEFT JOIN base.v_symbol_churn_90d base_ch
          ON base_ch.stable_symbol_id = n.stable_symbol_id"
    } else {
        ""
    };
    let churn_events_expr = if has_base_churn {
        "COALESCE(base_ch.events, 0)"
    } else {
        "0"
    };
    let churn_commits_expr = if has_base_churn {
        "COALESCE(base_ch.commits, 0)"
    } else {
        "0"
    };
    let churn_added_expr = if has_base_churn {
        "COALESCE(base_ch.added, 0)"
    } else {
        "0"
    };
    let churn_modified_expr = if has_base_churn {
        "COALESCE(base_ch.modified, 0)"
    } else {
        "0"
    };
    let churn_deleted_expr = if has_base_churn {
        "COALESCE(base_ch.deleted, 0)"
    } else {
        "0"
    };
    let churn_renamed_expr = if has_base_churn {
        "COALESCE(base_ch.renamed, 0)"
    } else {
        "0"
    };
    let churn_last_touched_expr = if has_base_churn {
        "base_ch.last_touched"
    } else {
        "CAST(NULL AS TIMESTAMP)"
    };

    let blast_join = if has_base_blast_radius {
        "LEFT JOIN base.v_blast_radius base_br
          ON base_br.stable_symbol_id = n.stable_symbol_id"
    } else {
        ""
    };
    let blast_caller_count_expr = if has_base_blast_radius {
        "COALESCE(base_br.caller_count, ib.callers, 0)"
    } else {
        "COALESCE(ib.callers, 0)"
    };
    let blast_hot_caller_count_expr = if has_base_blast_radius {
        "COALESCE(base_br.hot_caller_count, 0)"
    } else {
        "0"
    };
    let blast_caller_churn_expr = if has_base_blast_radius {
        "COALESCE(base_br.caller_churn_90d, 0)"
    } else {
        "0"
    };
    let blast_self_churn_expr = if has_base_blast_radius {
        "COALESCE(base_br.self_churn_90d, ch.events, 0)"
    } else {
        "COALESCE(ch.events, 0)"
    };
    let blast_self_last_touched_expr = if has_base_blast_radius {
        "COALESCE(base_br.self_last_touched, ch.last_touched)"
    } else {
        "ch.last_touched"
    };
    let blast_score_expr = if has_base_blast_radius {
        "base_br.blast_radius_score"
    } else {
        "CAST(NULL AS DOUBLE)"
    };

    let commit_files_sql = if has_base_commit_files {
        "SELECT cf.*
         FROM base.v_commit_files cf"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS VARCHAR) AS commit_sha,
           CAST(NULL AS VARCHAR) AS file_path
         WHERE false"
            .to_owned()
    };

    let file_cochange_sql = if has_base_file_cochange {
        "SELECT fc.*
         FROM base.v_file_cochange fc"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS VARCHAR) AS file_a,
           CAST(NULL AS VARCHAR) AS file_b,
           CAST(NULL AS BIGINT) AS cochange_count,
           CAST(NULL AS BOOLEAN) AS has_static_edge
         WHERE false"
            .to_owned()
    };

    let commit_classified_sql = if has_base_commit_classified {
        "SELECT cc.*
         FROM base.v_commit_classified cc"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS VARCHAR) AS sha,
           CAST(NULL AS TIMESTAMP) AS author_ts,
           CAST(NULL AS VARCHAR) AS summary,
           CAST(NULL AS VARCHAR) AS commit_type,
           CAST(NULL AS BOOLEAN) AS is_fix
         WHERE false"
            .to_owned()
    };

    let fix_hotspots_sql = if has_base_fix_hotspots {
        "SELECT fh.*
         FROM base.v_fix_hotspots fh"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS VARCHAR) AS file_path,
           CAST(NULL AS BIGINT) AS commits,
           CAST(NULL AS BIGINT) AS fix_commits,
           CAST(NULL AS DOUBLE) AS fix_pct
         WHERE false"
            .to_owned()
    };

    let hidden_coupling_sql = if has_base_hidden_coupling {
        "SELECT hc.*
         FROM base.v_hidden_coupling hc"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS VARCHAR) AS file_a,
           CAST(NULL AS VARCHAR) AS file_b,
           CAST(NULL AS BIGINT) AS cochange_count
         WHERE false"
            .to_owned()
    };

    let velocity_sql = if has_base_velocity {
        "SELECT vel.*
         FROM base.v_velocity vel"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS TIMESTAMP) AS month,
           CAST(NULL AS BIGINT) AS touches,
           CAST(NULL AS BIGINT) AS commits,
           CAST(NULL AS BIGINT) AS added,
           CAST(NULL AS BIGINT) AS deleted
         WHERE false"
            .to_owned()
    };

    let age_join = if has_base_age {
        "LEFT JOIN base.v_symbol_age base_age
          ON base_age.stable_symbol_id = n.stable_symbol_id"
    } else {
        ""
    };
    let age_born_expr = if has_base_age {
        "base_age.born"
    } else {
        "CAST(NULL AS TIMESTAMP)"
    };
    let age_last_seen_expr = if has_base_age {
        "base_age.last_seen"
    } else {
        "CAST(NULL AS TIMESTAMP)"
    };
    let age_lifespan_expr = if has_base_age {
        "base_age.lifespan_days"
    } else {
        "CAST(NULL AS BIGINT)"
    };
    let age_history_commits_expr = if has_base_age {
        "base_age.history_commits"
    } else {
        "CAST(NULL AS BIGINT)"
    };

    let genealogy_sql = if has_base_genealogy {
        "SELECT gen.*
         FROM base.v_symbol_genealogy gen
         JOIN nodes n
           ON n.stable_symbol_id = gen.stable_symbol_id"
            .to_owned()
    } else {
        "SELECT
           CAST(NULL AS VARCHAR) AS stable_symbol_id,
           CAST(NULL AS VARCHAR) AS commit_sha,
           CAST(NULL AS VARCHAR) AS change_kind,
           CAST(NULL AS VARCHAR) AS rename_prev_stable_symbol_id,
           CAST(NULL AS VARCHAR) AS rename_prev_commit
         WHERE false"
            .to_owned()
    };

    conn.execute_batch(&format!(
        r"
        CREATE OR REPLACE VIEW v_symbol_file AS
        {symbol_file_sql};

        CREATE OR REPLACE VIEW v_symbol_churn_90d AS
        SELECT
          n.stable_symbol_id,
          {churn_events_expr}::BIGINT AS events,
          {churn_commits_expr}::BIGINT AS commits,
          {churn_added_expr}::BIGINT AS added,
          {churn_modified_expr}::BIGINT AS modified,
          {churn_deleted_expr}::BIGINT AS deleted,
          {churn_renamed_expr}::BIGINT AS renamed,
          {churn_last_touched_expr} AS last_touched
        FROM nodes n
        {churn_join};

        CREATE OR REPLACE VIEW v_symbol_centrality AS
        SELECT
          n.stable_symbol_id,
          n.node_id,
          {pagerank_expr} AS pagerank,
          (
            SELECT count(*)
            FROM edges_by_dst e
            WHERE e.target_stable_id = n.stable_symbol_id
              AND e.edge_kind IN ('calls', 'calls_dyn')
          ) AS in_degree,
          (
            SELECT count(*)
            FROM edges e
            WHERE e.source_stable_id = n.stable_symbol_id
              AND e.edge_kind IN ('calls', 'calls_dyn')
          ) AS out_degree
        FROM nodes n
        {centrality_join};

        CREATE OR REPLACE VIEW v_symbol_component AS
        SELECT
          n.stable_symbol_id,
          n.node_id,
          {component_id_expr} AS component_id,
          {component_size_expr} AS component_size
        FROM nodes n
        {component_join};

        CREATE OR REPLACE VIEW v_symbol_community AS
        SELECT
          n.stable_symbol_id,
          n.node_id,
          {community_id_expr} AS community_id
        FROM nodes n
        {community_join};

        CREATE OR REPLACE VIEW v_symbol_inbound AS
        SELECT
          target_stable_id AS stable_symbol_id,
          sum(CASE WHEN edge_kind IN ('calls', 'calls_dyn') THEN 1 ELSE 0 END) AS callers,
          sum(CASE WHEN edge_kind = 'references_other' AND relation = 'imports' THEN 1 ELSE 0 END) AS importers,
          sum(CASE WHEN edge_kind = 'references_other' AND relation = 'contains' THEN 1 ELSE 0 END) AS containers,
          count(*) AS inbound_total
        FROM edges
        WHERE target_stable_id IS NOT NULL
        GROUP BY target_stable_id;

        CREATE OR REPLACE VIEW v_blast_radius AS
        SELECT
          n.stable_symbol_id,
          n.entity_name,
          n.symbol_kind,
          n.file_path,
          {blast_caller_count_expr}::BIGINT AS caller_count,
          {blast_hot_caller_count_expr}::BIGINT AS hot_caller_count,
          {blast_caller_churn_expr}::BIGINT AS caller_churn_90d,
          {blast_self_churn_expr}::BIGINT AS self_churn_90d,
          {blast_self_last_touched_expr} AS self_last_touched,
          {blast_score_expr} AS blast_radius_score
        FROM nodes n
        LEFT JOIN v_symbol_inbound ib USING (stable_symbol_id)
        LEFT JOIN v_symbol_churn_90d ch USING (stable_symbol_id)
        {blast_join};

        CREATE OR REPLACE VIEW v_commit_files AS
        {commit_files_sql};

        CREATE OR REPLACE VIEW v_file_static_edges AS
        SELECT DISTINCT
          na.file_path AS file_a,
          nb.file_path AS file_b
        FROM edges e
        JOIN nodes na
          ON na.stable_symbol_id = e.source_stable_id
        JOIN nodes nb
          ON nb.stable_symbol_id = e.target_stable_id
        WHERE na.file_path IS NOT NULL
          AND nb.file_path IS NOT NULL
          AND na.file_path != nb.file_path;

        CREATE OR REPLACE VIEW v_file_cochange AS
        {file_cochange_sql};

        CREATE OR REPLACE VIEW v_commit_classified AS
        {commit_classified_sql};

        CREATE OR REPLACE VIEW v_fix_hotspots AS
        {fix_hotspots_sql};

        CREATE OR REPLACE VIEW v_hidden_coupling AS
        {hidden_coupling_sql};

        CREATE OR REPLACE VIEW v_velocity AS
        {velocity_sql};

        CREATE OR REPLACE VIEW v_symbol_age AS
        SELECT
          n.stable_symbol_id,
          {age_born_expr} AS born,
          {age_last_seen_expr} AS last_seen,
          {age_lifespan_expr} AS lifespan_days,
          {age_history_commits_expr} AS history_commits
        FROM nodes n
        {age_join};

        CREATE OR REPLACE VIEW v_symbol_genealogy AS
        {genealogy_sql};

        CREATE OR REPLACE VIEW v_unresolved_hotspots AS
        SELECT target_label, edge_kind, count(*) AS sites
        FROM edges_unresolved
        WHERE target_label IS NOT NULL
        GROUP BY target_label, edge_kind;

        CREATE OR REPLACE VIEW v_symbol_risk AS
        SELECT
          n.stable_symbol_id,
          n.entity_name,
          n.symbol_kind,
          n.file_path,
          COALESCE(ct.pagerank, 0.0) AS pagerank,
          COALESCE(ct.in_degree, 0) AS in_degree,
          COALESCE(ct.out_degree, 0) AS out_degree,
          COALESCE(ch.events, 0) AS churn_90d,
          ch.last_touched,
          CASE
            WHEN COALESCE(ct.in_degree, 0) = 0 THEN 'leaf'
            WHEN COALESCE(ch.events, 0) = 0 THEN 'load-bearing wall'
            WHEN ch.events >= 10 THEN 'hot-central'
            ELSE 'active'
          END AS posture
        FROM nodes n
        LEFT JOIN v_symbol_centrality ct USING (stable_symbol_id)
        LEFT JOIN v_symbol_churn_90d ch USING (stable_symbol_id);

        CREATE OR REPLACE VIEW v_symbol_scorecard AS
        SELECT
          n.stable_symbol_id,
          n.entity_name,
          n.qualified_name,
          n.symbol_kind,
          n.file_path,
          COALESCE(ct.pagerank, 0.0) AS pagerank,
          COALESCE(ct.in_degree, 0) AS in_degree,
          COALESCE(ct.out_degree, 0) AS out_degree,
          cmp.component_id,
          cmp.component_size,
          comm.community_id,
          COALESCE(ib.callers, 0) AS callers,
          COALESCE(ib.importers, 0) AS importers,
          COALESCE(ib.inbound_total, 0) AS inbound_total,
          COALESCE(ch.events, 0) AS churn_90d,
          ch.last_touched,
          age.born,
          age.last_seen,
          age.lifespan_days,
          br.blast_radius_score,
          CASE
            WHEN COALESCE(ct.in_degree, 0) = 0 THEN 'leaf'
            WHEN COALESCE(ch.events, 0) = 0 THEN 'load-bearing wall'
            WHEN ch.events >= 10 THEN 'hot-central'
            ELSE 'active'
          END AS posture
        FROM nodes n
        LEFT JOIN v_symbol_centrality ct USING (stable_symbol_id)
        LEFT JOIN v_symbol_component cmp USING (stable_symbol_id)
        LEFT JOIN v_symbol_community comm USING (stable_symbol_id)
        LEFT JOIN v_symbol_inbound ib USING (stable_symbol_id)
        LEFT JOIN v_symbol_churn_90d ch USING (stable_symbol_id)
        LEFT JOIN v_symbol_age age USING (stable_symbol_id)
        LEFT JOIN v_blast_radius br USING (stable_symbol_id);

        CREATE OR REPLACE VIEW v_graph_metrics AS
        WITH graph_counts AS (
          SELECT
            (SELECT count(*) FROM edges WHERE edge_kind IN ('calls', 'calls_dyn')) AS calls_edges,
            (
              SELECT count(DISTINCT node_id)
              FROM (
                SELECT src_id AS node_id FROM edges WHERE src_id IS NOT NULL
                UNION
                SELECT dst_id AS node_id FROM edges WHERE dst_id IS NOT NULL
              ) connected
            ) AS connected_nodes,
            (SELECT count(*) FROM nodes) AS node_count
        )
        SELECT
          calls_edges,
          connected_nodes,
          {components_expr} AS components,
          {largest_component_expr} AS largest_component,
          {communities_expr} AS communities,
          CASE
            WHEN node_count <= 1 THEN 0.0
            ELSE calls_edges::DOUBLE / (node_count::DOUBLE * (node_count::DOUBLE - 1.0))
          END AS density
        FROM graph_counts;

        CREATE OR REPLACE VIEW v_catalog AS
        SELECT * FROM (VALUES
          ('v_symbol_scorecard',   'symbol',   'overlay per-symbol row: merged structure plus base temporal history'),
          ('v_symbol_risk',        'symbol',   'overlay centrality x base churn posture'),
          ('v_symbol_centrality',  'symbol',   'overlay merged in/out degree plus base PageRank when available'),
          ('v_symbol_component',   'symbol',   'base component when available, singleton for overlay-only symbols'),
          ('v_symbol_community',   'symbol',   'base community when available, singleton for overlay-only symbols'),
          ('v_symbol_age',         'symbol',   'base age for live symbols, NULL for overlay-only symbols'),
          ('v_symbol_genealogy',   'symbol',   'base rename trails for live symbols'),
          ('v_symbol_churn_90d',   'symbol',   'base 90-day churn for live symbols, zero for overlay-only symbols'),
          ('v_symbol_inbound',     'symbol',   'merged inbound callers / importers / containers'),
          ('v_blast_radius',       'symbol',   'base blast-radius for live symbols, structural fallback for overlay-only symbols'),
          ('v_hidden_coupling',    'file',     'base hidden-coupling surface when available'),
          ('v_file_cochange',      'file',     'base file co-change surface when available'),
          ('v_fix_hotspots',       'file',     'base fix-hotspot surface when available'),
          ('v_commit_classified',  'commit',   'base conventional-commit surface when available'),
          ('v_velocity',           'temporal', 'base temporal velocity surface when available'),
          ('v_unresolved_hotspots','edge',     'merged unresolved call labels by site count'),
          ('v_graph_metrics',      'graph',    'overlay merged structural graph metrics')
        ) AS t(view_name, grain, purpose);
        "
    ))
    .context("failed to create worktree overlay analytical views")
}

fn base_relation_exists(conn: &duckdb::Connection, relation_name: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT count(*) \
             FROM information_schema.tables \
             WHERE table_catalog = 'base' \
               AND table_name = ?",
            duckdb::params![relation_name],
            |row| row.get(0),
        )
        .context("failed to inspect attached base analyst catalog")?;
    Ok(count > 0)
}
