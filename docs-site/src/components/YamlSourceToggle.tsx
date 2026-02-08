import React from 'react';
import Details from '@theme/Details';
import CodeBlock from '@theme/CodeBlock';

interface YamlSourceToggleProps {
  yaml: string;
  filename: string;
}

export function YamlSourceToggle({yaml, filename}: YamlSourceToggleProps): React.ReactElement {
  return (
    <Details summary={<summary>View YAML source: {filename}</summary>}>
      <CodeBlock language="yaml" title={filename}>
        {yaml}
      </CodeBlock>
    </Details>
  );
}
