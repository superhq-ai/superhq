interface Props {
    children: React.ReactNode;
    className?: string;
}

export default function Screen({ children, className = "" }: Props) {
    return (
        <div
            className={`h-full w-full flex flex-col bg-app-base text-app-text ${className}`}
            style={{
                paddingTop: "env(safe-area-inset-top)",
                paddingBottom: "env(safe-area-inset-bottom)",
            }}
        >
            {children}
        </div>
    );
}
