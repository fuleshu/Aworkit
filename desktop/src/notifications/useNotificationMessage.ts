import { useCallback, useState } from "react";

/** A repeated user action is a new occurrence even when its result text is identical. */
export function useNotificationMessage() {
  const [value, setValue] = useState({ message: null as string | null, occurrence: 0 });
  const setMessage = useCallback((message: string | null) => {
    setValue(current => ({ message, occurrence: current.occurrence + 1 }));
  }, []);
  return [value.message, setMessage, value.occurrence] as const;
}
