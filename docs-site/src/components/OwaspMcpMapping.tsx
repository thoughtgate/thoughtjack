import React from 'react';

interface OwaspMcpMappingProps {
  ids: string[];
}

function owaspMcpUrl(id: string): string {
  return `https://owasp.org/www-project-mcp-top-10/2025/${id}`;
}

export function OwaspMcpMapping({ids}: OwaspMcpMappingProps): React.ReactElement {
  return (
    <div className="tj-mapping tj-mapping--owasp">
      <strong>OWASP MCP:</strong>{' '}
      {ids.map((id) => (
        <a
          key={id}
          href={owaspMcpUrl(id)}
          target="_blank"
          rel="noopener noreferrer"
          className="tj-pill tj-pill--owasp"
        >
          {id}
        </a>
      ))}
    </div>
  );
}
