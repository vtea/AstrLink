import { toast } from "sonner";

type NotifyOptions = {
  id?: string;
  description?: string;
  duration?: number;
  action?: {
    label: string;
    onClick: () => void;
  };
};

export const notify = {
  success(message: string, options?: NotifyOptions) {
    toast.success(message, options);
  },
  error(message: string, options?: NotifyOptions) {
    toast.error(message, options);
  },
  warning(message: string, options?: NotifyOptions) {
    toast.warning(message, options);
  },
  dismiss(id?: string) {
    toast.dismiss(id);
  },
};
