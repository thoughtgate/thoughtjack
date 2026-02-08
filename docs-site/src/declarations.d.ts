declare module '@theme/Mermaid' {
  export interface MermaidProps {
    value: string;
  }
  export default function Mermaid(props: MermaidProps): React.ReactElement;
}

declare module '@theme/Details' {
  export interface DetailsProps {
    summary: React.ReactNode;
    children: React.ReactNode;
  }
  export default function Details(props: DetailsProps): React.ReactElement;
}

declare module '@theme/CodeBlock' {
  export interface CodeBlockProps {
    language?: string;
    title?: string;
    children: string;
  }
  export default function CodeBlock(props: CodeBlockProps): React.ReactElement;
}
