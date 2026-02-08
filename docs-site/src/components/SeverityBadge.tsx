import React from 'react';

type SeverityLevel = 'low' | 'medium' | 'high' | 'critical';

interface SeverityBadgeProps {
  level: SeverityLevel;
}

export function SeverityBadge({level}: SeverityBadgeProps): React.ReactElement {
  return (
    <span className={`tj-severity-badge tj-severity-badge--${level}`}>
      {level.toUpperCase()}
    </span>
  );
}
