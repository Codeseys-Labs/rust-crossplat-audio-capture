import { useMemo } from "react";
import { useAudioGraphStore } from "../store";

// TODO: Import and use react-force-graph-2d once dependencies are installed
// import ForceGraph2D from "react-force-graph-2d";

function KnowledgeGraphViewer() {
  const graphSnapshot = useAudioGraphStore((s) => s.graphSnapshot);

  const graphData = useMemo(() => {
    const nodes = graphSnapshot.entities.map((entity) => ({
      id: entity.id,
      name: entity.name,
      type: entity.entity_type,
      val: entity.mention_count,
    }));

    const links = graphSnapshot.relations.map((rel) => ({
      source: rel.source_id,
      target: rel.target_id,
      type: rel.relation_type,
    }));

    return { nodes, links };
  }, [graphSnapshot]);

  return (
    <div className="knowledge-graph-viewer">
      {graphData.nodes.length === 0 ? (
        <div className="graph-placeholder">
          <div className="graph-placeholder-icon">🔗</div>
          <p>Knowledge graph will appear here</p>
          <p className="panel-empty">Start capturing audio to build the graph</p>
        </div>
      ) : (
        <div className="graph-container">
          {/* TODO: Replace with ForceGraph2D component */}
          <p>Graph: {graphData.nodes.length} nodes, {graphData.links.length} links</p>
        </div>
      )}
    </div>
  );
}

export default KnowledgeGraphViewer;
