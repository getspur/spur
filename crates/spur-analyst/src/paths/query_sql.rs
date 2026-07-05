pub(super) fn duckpgq_direct_paths_sql(
    source_stable_id: &str,
    target_stable_id: &str,
    max_paths: usize,
) -> String {
    let source_sql = sql_string_literal(source_stable_id);
    let target_sql = sql_string_literal(target_stable_id);
    format!(
        "SELECT source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method \
         FROM GRAPH_TABLE (code \
           MATCH (a:duckpgq_nodes)-[e:duckpgq_edges]->(b:duckpgq_nodes) \
           WHERE a.stable_symbol_id = {source_sql} \
             AND b.stable_symbol_id = {target_sql} \
           COLUMNS (a.stable_symbol_id AS source_stable_id, \
                    b.stable_symbol_id AS target_stable_id, \
                    e.relation AS relation, \
                    e.edge_kind AS edge_kind, \
                    e.confidence AS confidence, \
                    e.bind_method AS bind_method)) \
         LIMIT {max_paths}"
    )
}

pub(super) fn duckpgq_shortest_hops_sql(
    source_stable_id: &str,
    target_stable_id: &str,
    max_hops: usize,
) -> String {
    let source_sql = sql_string_literal(source_stable_id);
    let target_sql = sql_string_literal(target_stable_id);
    format!(
        "SELECT hops \
         FROM GRAPH_TABLE (code \
           MATCH p = ANY SHORTEST (a:duckpgq_nodes)-[e:duckpgq_edges]->{{1,{max_hops}}}(b:duckpgq_nodes) \
           WHERE a.stable_symbol_id = {source_sql} \
             AND b.stable_symbol_id = {target_sql} \
           COLUMNS (path_length(p) AS hops)) \
         LIMIT 1"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecursivePathMode {
    Directed,
    Undirected,
}

impl RecursivePathMode {
    fn edge_source(self) -> &'static str {
        match self {
            Self::Directed => "traversable_edges",
            Self::Undirected => "edges_undirected",
        }
    }
}

pub(super) fn recursive_path_sql(
    max_hops: usize,
    max_paths: usize,
    mode: RecursivePathMode,
) -> String {
    let undirected_edges = match mode {
        RecursivePathMode::Directed => String::new(),
        RecursivePathMode::Undirected => format!(", {}", undirected_edges_cte()),
    };
    format!(
        "WITH RECURSIVE {traversable_edges}{undirected_edges}, {walk}, {complete_paths}, \
         {path_edges}, {ranked_edges} {select_rows}",
        traversable_edges = traversable_edges_cte(),
        walk = walk_cte(mode.edge_source(), max_hops),
        complete_paths = complete_paths_cte(max_paths),
        path_edges = path_edges_cte(),
        ranked_edges = ranked_edges_cte(mode),
        select_rows = select_path_rows(mode),
    )
}

fn traversable_edges_cte() -> &'static str {
    "traversable_edges AS ( \
       SELECT source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method \
       FROM edges \
       WHERE (relation = 'calls' \
              AND edge_kind IN ('calls', 'calls_dyn', 'references_hof')) \
          OR (relation = 'imports' AND bind_method = 'singleton') \
     )"
}

fn undirected_edges_cte() -> &'static str {
    "edges_undirected AS ( \
       SELECT source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method, 'forward' AS direction FROM traversable_edges \
       UNION ALL \
       SELECT target_stable_id AS source_stable_id, source_stable_id AS target_stable_id, relation, edge_kind, confidence, bind_method, 'reverse' AS direction FROM traversable_edges \
     )"
}

fn walk_cte(edge_source: &str, max_hops: usize) -> String {
    format!(
        "walk(current_id, depth, node_path, sort_key) AS ( \
           SELECT ?1::VARCHAR AS current_id, 0::INTEGER AS depth, [?1::VARCHAR] AS node_path, ?1::VARCHAR AS sort_key \
           UNION ALL \
           SELECT e.target_stable_id, w.depth + 1, list_append(w.node_path, e.target_stable_id), \
                  w.sort_key || '>' || e.target_stable_id \
           FROM walk w \
           JOIN {edge_source} e ON e.source_stable_id = w.current_id \
           WHERE w.depth < {max_hops} \
             AND e.target_stable_id IS NOT NULL \
             AND NOT list_contains(w.node_path, e.target_stable_id) \
         )"
    )
}

fn complete_paths_cte(max_paths: usize) -> String {
    format!(
        "complete_paths AS ( \
           SELECT row_number() OVER (ORDER BY depth, sort_key) - 1 AS path_index, depth, node_path \
           FROM ( \
             SELECT DISTINCT depth, node_path, sort_key \
             FROM walk \
             WHERE current_id = ?2 AND depth > 0 \
           ) \
           ORDER BY depth, sort_key \
           LIMIT {max_paths} \
         )"
    )
}

fn path_edges_cte() -> &'static str {
    "path_edges AS ( \
       SELECT path_index, idx - 1 AS hop_index, \
              list_extract(node_path, idx) AS source_stable_id, \
              list_extract(node_path, idx + 1) AS target_stable_id \
       FROM complete_paths \
       CROSS JOIN range(1, depth + 1) AS r(idx) \
     )"
}

fn ranked_edges_cte(mode: RecursivePathMode) -> String {
    let direction_column = match mode {
        RecursivePathMode::Directed => "",
        RecursivePathMode::Undirected => ", e.direction",
    };
    format!(
        "ranked_edges AS ( \
           SELECT pe.path_index, pe.hop_index, e.source_stable_id, e.target_stable_id, \
                  e.relation, e.edge_kind, e.confidence, e.bind_method{direction_column}, \
                  row_number() OVER ( \
                    PARTITION BY pe.path_index, pe.hop_index \
                    ORDER BY e.relation, e.edge_kind, e.confidence, e.bind_method \
                  ) AS edge_rank \
           FROM path_edges pe \
           JOIN {edge_source} e \
             ON e.source_stable_id = pe.source_stable_id \
            AND e.target_stable_id = pe.target_stable_id \
         )",
        edge_source = mode.edge_source(),
    )
}

fn select_path_rows(mode: RecursivePathMode) -> &'static str {
    match mode {
        RecursivePathMode::Directed => {
            "SELECT path_index, hop_index, source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method \
             FROM ranked_edges \
             WHERE edge_rank = 1 \
             ORDER BY path_index, hop_index"
        }
        RecursivePathMode::Undirected => {
            "SELECT path_index, hop_index, source_stable_id, target_stable_id, relation, edge_kind, confidence, bind_method, direction \
             FROM ranked_edges \
             WHERE edge_rank = 1 \
             ORDER BY path_index, hop_index"
        }
    }
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
