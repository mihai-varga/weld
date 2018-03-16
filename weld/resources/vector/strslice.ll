; Get a new str object from offset to \0
define %{NAME} @{NAME}.strslice(%{NAME} %vec, i64 %offset) {{
entry:
  %elements = extractvalue %{NAME} %vec, 0
  %newElements = getelementptr {ELEM}, {ELEM}* %elements, i64 %offset
  br label %body

body:
  %i = phi i64 [ 0, %entry ], [ %i2, %body ]
  %ptr = getelementptr {ELEM}, {ELEM}* %newElements, i64 %i
  %elem = load {ELEM}, {ELEM}* %ptr
  %i2 = add i64 %i, 1
  %cond = icmp ne {ELEM} 0, %elem
  br i1 %cond, label %body, label %done

done:
  %size = sub i64 %i2, 1
  %0 = insertvalue %{NAME} undef, {ELEM}* %newElements, 0
  %1 = insertvalue %{NAME} %0, i64 %size, 1
  ret %{NAME} %1
}}
