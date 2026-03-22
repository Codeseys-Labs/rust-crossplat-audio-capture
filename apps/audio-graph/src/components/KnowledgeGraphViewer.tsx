import { useRef, useState, useEffect, useCallback, useMemo } from "react";
import ForceGraph2D, {
  type ForceGraphMethods,
  type NodeObject,
  type LinkObject,
} from "react-force-graph-2d";
import { useAudioGraphStore } from "../store";
import type { GraphNode, GraphLink } from "../types";
import { formatTime } from "../utils/format";

/** Compute node radius from val. */
function nodeRadius(val: number): number {
  const r = Math.sqrt(val) * 3 + 4;
  return Math.max(4, Math.min(24, r));
}

function KnowledgeGraphViewer() {
  const graphSnapshot = useAudioGraphStore((s) => s.graphSnapshot);

  // ResizeObserver for auto-sizing to parent container
  const containerRef = useRef<HTMLDivElement>(null);
  const graphRef = useRef<ForceGraphMethods | undefined>(undefined);
  const [dimensions, setDimensions] = useState({ width: 600, height: 400 });

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const { width, height } = entry.contentRect;
        if (width > 0 && height > 0) {
          setDimensions({ width: Math.floor(width), height: Math.floor(height) });
        }
      }
    });

    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // Highlight state — track clicked node
  const [highlightNodeId, setHighlightNodeId] = useState<string | null>(null);
  const [highlightNeighbors, setHighlightNeighbors] = useState<Set<string>>(
    new Set()
  );

  // Build neighbor lookup once per snapshot
  const neighborMap = useMemo(() => {
    const map = new Map<string, Set<string>>();
    for (const link of graphSnapshot.links) {
      const src =
        typeof link.source === "object"
          ? (link.source as GraphNode).id
          : link.source;
      const tgt =
        typeof link.target === "object"
          ? (link.target as GraphNode).id
          : link.target;
      if (!map.has(src)) map.set(src, new Set());
      if (!map.has(tgt)) map.set(tgt, new Set());
      map.get(src)!.add(tgt);
      map.get(tgt)!.add(src);
    }
    return map;
  }, [graphSnapshot.links]);

  // Graph data — stable reference for react-force-graph
  const graphData = useMemo(
    () => ({
      nodes: graphSnapshot.nodes as NodeObject[],
      links: graphSnapshot.links as unknown as LinkObject[],
    }),
    [graphSnapshot.nodes, graphSnapshot.links]
  );

  // Click on a node → highlight it + neighbors
  const handleNodeClick = useCallback(
    (node: NodeObject) => {
      const id = node.id as string;
      if (highlightNodeId === id) {
        setHighlightNodeId(null);
        setHighlightNeighbors(new Set());
      } else {
        setHighlightNodeId(id);
        setHighlightNeighbors(neighborMap.get(id) ?? new Set());
      }
    },
    [highlightNodeId, neighborMap]
  );

  // Click on background → reset highlight
  const handleBackgroundClick = useCallback(() => {
    setHighlightNodeId(null);
    setHighlightNeighbors(new Set());
  }, []);

  // Custom node canvas rendering
  const nodeCanvasObject = useCallback(
    (node: NodeObject, ctx: CanvasRenderingContext2D, globalScale: number) => {
      const gNode = node as NodeObject & GraphNode;
      const x = node.x ?? 0;
      const y = node.y ?? 0;
      const r = nodeRadius(gNode.val ?? 1);

      // Determine dim state when a node is highlighted
      const isDimmed =
        highlightNodeId !== null &&
        highlightNodeId !== gNode.id &&
        !highlightNeighbors.has(gNode.id);

      const alpha = isDimmed ? 0.15 : 1;

      // Draw circle
      ctx.beginPath();
      ctx.arc(x, y, r, 0, 2 * Math.PI, false);
      ctx.globalAlpha = alpha;
      ctx.fillStyle = gNode.color || "#6b7280";
      ctx.fill();

      // Highlight ring on selected node
      if (highlightNodeId === gNode.id) {
        ctx.strokeStyle = "#ffffff";
        ctx.lineWidth = 2;
        ctx.stroke();
      }

      // Label — show when zoomed in or node is selected
      const fontSize = Math.max(10 / globalScale, 3);
      if (globalScale >= 0.6 || highlightNodeId === gNode.id) {
        ctx.font = `${fontSize}px sans-serif`;
        ctx.textAlign = "center";
        ctx.textBaseline = "top";
        ctx.fillStyle = `rgba(232, 232, 232, ${alpha})`;
        ctx.fillText(gNode.name, x, y + r + 2);
      }

      ctx.globalAlpha = 1;
    },
    [highlightNodeId, highlightNeighbors]
  );

  // Node pointer area for hit detection
  const nodePointerAreaPaint = useCallback(
    (node: NodeObject, color: string, ctx: CanvasRenderingContext2D) => {
      const gNode = node as NodeObject & GraphNode;
      const x = node.x ?? 0;
      const y = node.y ?? 0;
      const r = nodeRadius(gNode.val ?? 1) + 2;
      ctx.beginPath();
      ctx.arc(x, y, r, 0, 2 * Math.PI, false);
      ctx.fillStyle = color;
      ctx.fill();
    },
    []
  );

  // Link width based on weight
  const linkWidth = useCallback((link: LinkObject) => {
    const gLink = link as LinkObject & GraphLink;
    return Math.sqrt(gLink.weight ?? 1) + 0.5;
  }, []);

  // Link color with transparency and dimming
  const linkColor = useCallback(
    (link: LinkObject) => {
      const gLink = link as LinkObject & GraphLink;
      const base = gLink.color || "#6b7280";

      if (highlightNodeId !== null) {
        const src =
          typeof gLink.source === "object"
            ? (gLink.source as GraphNode).id
            : gLink.source;
        const tgt =
          typeof gLink.target === "object"
            ? (gLink.target as GraphNode).id
            : gLink.target;
        if (src !== highlightNodeId && tgt !== highlightNodeId) {
          return `${base}15`;
        }
      }

      return `${base}99`;
    },
    [highlightNodeId]
  );

  // Link label (shown on hover)
  const linkLabel = useCallback((link: LinkObject) => {
    const gLink = link as LinkObject & GraphLink;
    return gLink.relation_type ?? "";
  }, []);

  // Node tooltip (HTML)
  const nodeLabel = useCallback((node: NodeObject) => {
    const gNode = node as NodeObject & GraphNode;
    const parts = [
      `<strong>${gNode.name}</strong>`,
      `Type: ${gNode.entity_type}`,
      `Mentions: ${gNode.mention_count}`,
    ];
    if (gNode.description) parts.push(gNode.description);
    parts.push(`First seen: ${formatTime(gNode.first_seen)}`);
    parts.push(`Last seen: ${formatTime(gNode.last_seen)}`);
    return parts.join("<br/>");
  }, []);

  const hasNodes = graphSnapshot.nodes.length > 0;
  const { total_nodes, total_edges } = graphSnapshot.stats;

  return (
    <div className="graph-viewer" ref={containerRef}>
      {!hasNodes ? (
        <div className="graph-viewer__empty" role="status">
          <div className="graph-viewer__empty-icon" aria-hidden="true">
            ◉
          </div>
          <p className="graph-viewer__empty-text">
            Start capturing audio to build the knowledge graph
          </p>
        </div>
      ) : (
        <div className="graph-viewer__container">
          <ForceGraph2D
            ref={graphRef as React.MutableRefObject<ForceGraphMethods | undefined>}
            graphData={graphData}
            width={dimensions.width}
            height={dimensions.height}
            backgroundColor="transparent"
            nodeCanvasObject={nodeCanvasObject}
            nodePointerAreaPaint={nodePointerAreaPaint}
            nodeLabel={nodeLabel}
            onNodeClick={handleNodeClick}
            onBackgroundClick={handleBackgroundClick}
            linkWidth={linkWidth}
            linkColor={linkColor}
            linkLabel={linkLabel}
            linkDirectionalArrowLength={4}
            linkDirectionalArrowRelPos={1}
            cooldownTicks={100}
            d3AlphaDecay={0.02}
            d3VelocityDecay={0.3}
            enableZoomInteraction={true}
            enablePanInteraction={true}
          />
        </div>
      )}

      {hasNodes && (
        <div className="graph-viewer__stats" aria-live="polite">
          <span>Nodes: {total_nodes}</span>
          <span className="graph-viewer__stats-sep">|</span>
          <span>Edges: {total_edges}</span>
        </div>
      )}
    </div>
  );
}

export default KnowledgeGraphViewer;
