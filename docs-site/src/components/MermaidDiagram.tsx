import React from 'react';
import Mermaid from '@theme/Mermaid';

interface MermaidDiagramProps {
  chart: string;
  title?: string;
}

export function MermaidDiagram({chart, title}: MermaidDiagramProps): React.ReactElement {
  return (
    <div className="tj-mermaid-container">
      {title && <div className="tj-mermaid-title">{title}</div>}
      <Mermaid value={chart} />
    </div>
  );
}
