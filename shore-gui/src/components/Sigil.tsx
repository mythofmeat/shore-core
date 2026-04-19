interface SigilProps {
  streaming?: boolean;
  className?: string;
}

export function Sigil({ streaming, className }: SigilProps) {
  return (
    <span className={`sigil ${streaming ? "streaming" : ""} ${className ?? ""}`}>
      ✦
    </span>
  );
}
