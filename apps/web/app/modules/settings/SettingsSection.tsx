interface SettingsSectionProps {
  title: string;
  children: React.ReactNode;
}

export function SettingsSection({ title, children }: SettingsSectionProps) {
  return (
    <div className="bg-card rounded-xl shadow-sm border border-border overflow-hidden mb-6">
      <div className="p-4 border-b border-border bg-muted/50">
        <h2 className="font-heading font-semibold text-foreground">{title}</h2>
      </div>
      <div className="p-4">{children}</div>
    </div>
  );
}
