import React from 'react';

interface MitreMappingProps {
  tactics?: string[];
  techniques?: string[];
}

function mitreTechniqueUrl(id: string): string {
  // T1195.002 → /techniques/T1195/002/
  const parts = id.split('.');
  return `https://attack.mitre.org/techniques/${parts.join('/')}/`;
}

function mitreTacticUrl(id: string): string {
  return `https://attack.mitre.org/tactics/${id}/`;
}

export function MitreMapping({tactics = [], techniques = []}: MitreMappingProps): React.ReactElement {
  return (
    <div className="tj-mapping tj-mapping--mitre">
      {tactics.length > 0 && (
        <div className="tj-mapping-section">
          <strong>Tactics:</strong>{' '}
          {tactics.map((id) => (
            <a
              key={id}
              href={mitreTacticUrl(id)}
              target="_blank"
              rel="noopener noreferrer"
              className="tj-pill tj-pill--mitre"
            >
              {id}
            </a>
          ))}
        </div>
      )}
      {techniques.length > 0 && (
        <div className="tj-mapping-section">
          <strong>Techniques:</strong>{' '}
          {techniques.map((id) => (
            <a
              key={id}
              href={mitreTechniqueUrl(id)}
              target="_blank"
              rel="noopener noreferrer"
              className="tj-pill tj-pill--mitre"
            >
              {id}
            </a>
          ))}
        </div>
      )}
    </div>
  );
}
