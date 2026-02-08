import React from 'react';
import {SeverityBadge} from './SeverityBadge';
import {MitreMapping} from './MitreMapping';
import {OwaspMcpMapping} from './OwaspMcpMapping';

type SeverityLevel = 'low' | 'medium' | 'high' | 'critical';

interface AttackMetadataCardProps {
  id: string;
  severity: string;
  tactics?: string[];
  techniques?: string[];
  owaspIds?: string[];
}

export function AttackMetadataCard({
  id,
  severity,
  tactics,
  techniques,
  owaspIds,
}: AttackMetadataCardProps): React.ReactElement {
  return (
    <div className="tj-metadata-card">
      <div className="tj-metadata-card__header">
        <code className="tj-metadata-card__id">{id}</code>
        <SeverityBadge level={severity as SeverityLevel} />
      </div>
      {(tactics || techniques) && (
        <div className="tj-metadata-card__section">
          <MitreMapping tactics={tactics} techniques={techniques} />
        </div>
      )}
      {owaspIds && owaspIds.length > 0 && (
        <div className="tj-metadata-card__section">
          <OwaspMcpMapping ids={owaspIds} />
        </div>
      )}
    </div>
  );
}
