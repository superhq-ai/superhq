import { useNavigate } from "react-router";

interface Props {
    title: string;
    subtitle?: string;
    back?: boolean;
    trailing?: React.ReactNode;
}

export default function TopBar({ title, subtitle, back = false, trailing }: Props) {
    const navigate = useNavigate();
    return (
        <div className="flex h-11 shrink-0 items-center gap-1 px-2">
            {back ? (
                <button
                    onClick={() => navigate(-1)}
                    className="flex h-9 w-9 items-center justify-center rounded-xl text-app-text-muted active:bg-app-hover"
                    aria-label="Back"
                >
                    <svg
                        width="14"
                        height="14"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth={2}
                        strokeLinecap="round"
                        strokeLinejoin="round"
                    >
                        <path d="M15 18l-6-6 6-6" />
                    </svg>
                </button>
            ) : null}
            <div className="flex min-w-0 flex-1 flex-col px-1">
                <div className="truncate text-[15px] font-medium text-app-text">
                    {title}
                </div>
                {subtitle ? (
                    <div className="truncate text-[11px] text-app-text-ghost">
                        {subtitle}
                    </div>
                ) : null}
            </div>
            {trailing ? <div className="flex shrink-0 items-center gap-1">{trailing}</div> : null}
        </div>
    );
}
